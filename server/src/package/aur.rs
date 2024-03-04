use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::anyhow;
use raur::Raur;

// this struct represents information about a package in the aur
pub struct AurInfo {
    // base name of the package
    pub base: String,
    // repository of the package source
    pub repository: String,
    // is development package
    pub devel: bool
}

/// finds a package in the aur
pub async fn find(name: &str) -> anyhow::Result<Option<AurInfo>> {
    let raur = raur::Handle::new();
    let pkg = raur.info(&[name]).await?;

    if let Some(info) = pkg.first() {
        Ok(Some(AurInfo {
            base: info.package_base.clone(),
            repository: to_aur_git(&info.package_base),
            devel: info.package_base.ends_with("-git")
        }))
    } else {
        Ok(None)
    }
}

fn to_aur_git(base: &str) -> String {
    format!("https://aur.archlinux.org/{base}.git")
}

/// Returns the srcinfo string for a pkgbuild located in the given directory
/// TODO: This method of using makepkg quite dubious, as it switches to another user just for that. Improve this!
pub async fn generate_srcinfo_string(pkgbuild: &Path) -> anyhow::Result<String> {
    let dir = PathBuf::from("/tmp").join(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string());

    tokio::fs::create_dir(&dir).await?;
    tokio::fs::copy(pkgbuild, dir.join("PKGBUILD")).await?;

    let uid_output = tokio::process::Command::new("id").arg("-u").output().await?;
    let uid = String::from_utf8_lossy(&uid_output.stdout);

    // detect whether running in container (as root)
    let status = if uid.trim() == "0" {
        tokio::fs::set_permissions(&dir, Permissions::from_mode(0o777)).await?;
        tokio::fs::set_permissions(dir.join("PKGBUILD"), Permissions::from_mode(0o777)).await?;

        tokio::process::Command::new("su")
            .arg("user")
            .arg("sh")
            .arg("-c")
            .arg("makepkg --printsrcinfo")
            .current_dir(&dir)
            .output().await?
    } else {
        tokio::process::Command::new("makepkg").arg("--printsrcinfo")
            .current_dir(&dir)
            .output().await?
    };

    tokio::fs::remove_dir_all(dir).await?;

    if !status.status.success() { Err(anyhow!("failed generate srcinfo: {}", String::from_utf8_lossy(&status.stderr))) }
    else {
        Ok(String::from_utf8_lossy(&status.stdout).to_string())
    }
}