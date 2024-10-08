use crate::build::schedule::BuildScheduler;
use crate::config::{CLI_PACKAGE_NAME, CONFIG};
use crate::database::Database;
use crate::package::source::cli::SereneCliSource;
use crate::package::source::{Source, SrcinfoWrapper};
use crate::runner;
use crate::runner::archive;
use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use hyper::Body;
use log::info;
use serene_data::build::BuildReason;
use serene_data::package::MakepkgFlag;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

pub mod aur;
pub mod git;
pub mod source;

const SOURCE_FOLDER: &str = "sources";

const PACKAGE_EXTENSION: &str = ".pkg.tar.zst"; // see /etc/makepkg.conf

fn get_folder_tmp() -> PathBuf {
    Path::new(SOURCE_FOLDER)
        .join("tmp")
        .join(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string())
}

/// adds a source to the package store as a package, returns none if base is
/// already present, otherwise the base is returned this is able to replace a
/// source of a given package if it already exists
pub async fn add_source(
    db: &Database,
    mut source: Box<dyn Source + Sync + Send>,
    replace: bool,
) -> anyhow::Result<Option<Package>> {
    let folder = get_folder_tmp();
    fs::create_dir_all(&folder).await?;

    let result = 'create: {
        // pull source
        if let Err(e) = source.create(&folder).await {
            break 'create Err(anyhow!("failed to check out source: {e:?}"));
        }

        // get srcinfo
        let srcinfo = match source.get_srcinfo(&folder).await {
            Ok(s) => s,
            Err(e) => break 'create Err(e),
        };

        // check other packages
        let (package, new) =
            if let Some(mut package) = Package::find(&srcinfo.base.pkgbase, db).await? {
                // only proceed if replacing enabled
                if !replace {
                    break 'create Ok(None);
                }

                if let Err(e) = package.self_destruct().await {
                    break 'create Err(anyhow!("failed to remove old source: {e:#}"));
                }

                package.source = source;

                (package, false)
            } else {
                // create package
                (
                    Package {
                        base: srcinfo.base.pkgbase.clone(),
                        added: Utc::now(),

                        clean: !source.is_devel(),
                        enabled: true,
                        schedule: None,
                        prepare: None,
                        flags: vec![],

                        version: None,
                        srcinfo: None,
                        pkgbuild: None,

                        source,
                    },
                    true,
                )
            };

        // move package
        if let Err(e) = fs::rename(&folder, package.get_folder()).await {
            break 'create Err(anyhow!("failed to copy source: {e:#}"));
        }

        Ok(Some((package, new)))
    };

    if let Ok(Some((p, new))) = &result {
        // store on success
        if *new {
            p.save(db).await?
        } else {
            p.change_sources(db).await?
        }
    } else {
        // cleanup when failed
        fs::remove_dir_all(folder).await?;
    }

    result.map(|o| o.map(|(p, _)| p))
}

/// adds the cli to the current packages
pub async fn try_add_cli(db: &Database, scheduler: &mut BuildScheduler) -> anyhow::Result<()> {
    if Package::has(CLI_PACKAGE_NAME, db).await? {
        return Ok(());
    }

    info!("adding and building serene-cli");
    if let Some(mut package) = add_source(db, Box::new(SereneCliSource::new()), false).await? {
        package.clean = true;
        package.change_settings(db).await?;

        scheduler.schedule(&package).await?;
        scheduler.run(&package, true, BuildReason::Initial).await?;

        info!("successfully added serene-cli");
    }

    Ok(())
}

/// this struct represents a package built by serene
#[derive(Clone)]
pub struct Package {
    /// base of the package
    pub base: String,
    /// time when the package was added
    pub added: DateTime<Utc>,

    /// source of the package
    pub source: Box<dyn Source + Sync + Send>,

    /// pkgbuild string used for the currently passing build for user pleasure
    pub pkgbuild: Option<String>,
    /// srcinfo of the current build, reported from the package for devel
    /// packages
    pub srcinfo: Option<SrcinfoWrapper>,
    /// DEPRECATED: version of the current build of the package
    pub version: Option<String>,

    /// whether package is enabled, meaning it is built automatically
    pub enabled: bool,
    /// whether package should be cleaned after building
    pub clean: bool,
    /// potential custom cron schedule string
    pub schedule: Option<String>,
    /// commands to run in container before package build, they are written to
    /// the shell
    pub prepare: Option<String>,
    /// special makepkg flags
    pub flags: Vec<MakepkgFlag>,
}

impl Package {
    /// gets the current folder for the source for the package
    fn get_folder(&self) -> PathBuf {
        Path::new(SOURCE_FOLDER).join(&self.base)
    }

    /// gets the schedule string for the package
    pub fn get_schedule(&self) -> String {
        self.schedule
            .as_ref()
            .unwrap_or_else(|| {
                if self.source.is_devel() {
                    &CONFIG.schedule_devel
                } else {
                    &CONFIG.schedule_default
                }
            })
            .clone()
    }

    pub async fn updatable(&self) -> anyhow::Result<bool> {
        self.source.update_available().await
    }

    pub async fn update(&mut self) -> anyhow::Result<()> {
        self.source.update(&self.get_folder()).await
    }

    /// upgrades the version of the package
    /// returns an error if a version mismatch is detected with the source files
    pub async fn upgrade(&mut self, reported: SrcinfoWrapper) -> anyhow::Result<()> {
        let mut srcinfo = self.source.get_srcinfo(&self.get_folder()).await?;
        let pkgbuild = self.source.get_pkgbuild(&self.get_folder()).await?;

        if self.source.is_devel() {
            // upgrade devel package srcinfo to reflect version and rel
            srcinfo = reported;
        } else if srcinfo.base.pkgver != reported.base.pkgver {
            // check for version mismatch for non-devel packages
            return Err(anyhow!(
                "version mismatch on package {}, expected {} but built {}",
                &self.base,
                &srcinfo.base.pkgver,
                &reported.base.pkgver
            ));
        }

        self.version = Some(srcinfo.base.pkgver.clone());
        self.srcinfo = Some(srcinfo);
        self.pkgbuild = Some(pkgbuild);

        Ok(())
    }

    /// returns the expected built files
    /// requires the version to be upgraded
    pub async fn expected_files(&self) -> anyhow::Result<Vec<String>> {
        let srcinfo = self.srcinfo.as_ref().ok_or(anyhow!(
            "no srcinfo loaded, upgrade version first. this is an internal error, please report"
        ))?;

        let rel = &srcinfo.base.pkgrel;
        let version = &srcinfo.base.pkgver;
        let epoch = srcinfo
            .base
            .epoch
            .as_ref()
            .map(|s| format!("{}:", s))
            .unwrap_or_else(|| "".to_string());
        let arch = select_arch(&srcinfo.pkg.arch);

        Ok(srcinfo
            .names()
            .map(|s| format!("{s}-{epoch}{version}-{rel}-{arch}{PACKAGE_EXTENSION}"))
            .collect())
    }

    pub async fn build_files(&self) -> anyhow::Result<Body> {
        let mut archive = archive::begin_write();

        // upload sources
        self.source.load_build_files(&self.get_folder(), &mut archive).await?;

        // upload repository file
        archive::write_file(runner::repository_file(), "custom-repo", false, &mut archive).await?;

        // upload prepare script
        archive::write_file(
            self.prepare.clone().unwrap_or_default(),
            "serene-prepare.sh",
            false,
            &mut archive,
        )
        .await?;

        // upload makepkg flags
        archive::write_file(
            self.flags.iter().map(|f| format!("--{f} ")).collect::<String>(),
            "makepkg-flags",
            false,
            &mut archive,
        )
        .await?;

        archive::end_write(archive).await
    }

    /// removes the source files of the source
    pub async fn self_destruct(&self) -> anyhow::Result<()> {
        fs::remove_dir_all(self.get_folder()).await.context("could not delete source directory")
    }

    pub fn get_packages(&self) -> Vec<String> {
        self.srcinfo
            .as_ref()
            .map(|s| s.names().map(|s| s.to_owned()).collect())
            .unwrap_or_else(|| vec![])
    }
}

/// selects the built architecture from a list of architectures
fn select_arch(available: &Vec<String>) -> String {
    // system can only build either itself or any
    if available.iter().any(|s| s == &CONFIG.architecture) {
        CONFIG.architecture.to_owned()
    } else {
        "any".to_string()
    }
}
