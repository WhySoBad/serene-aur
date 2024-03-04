use clap::{Parser, Subcommand, ArgAction};

#[derive(Parser)]
#[clap(version, about)]
#[command(disable_help_subcommand = true)]
pub struct Args {
    #[clap(subcommand)]
    pub command: Command,

    /// override the host url that is used
    #[clap(short, long)]
    pub server: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// list all packages which are added
    List,

    /// adds a package
    Add {
        /// base name for the aur package or custom url
        name: String,

        /// name is custom repository
        #[clap(short, long)]
        custom: bool,

        /// is development package, only works on custom urls
        #[clap(short, long)]
        devel: bool,
    },

    /// removes a package
    Remove {
        /// base name of the package
        name: String
    },

    /// schedules an immediate build for a package
    Build {
        /// base name of the package
        name: String
    },

    /// get and set info about a package
    Info {
        /// base name of the package
        name: String,

        /// show all builds
        #[clap(short, long)]
        all: bool,

        /// what type of info to get
        #[clap(subcommand)]
        what: Option<InfoCommand>
    },

    /// prints the current secret
    Secret {
        /// print the secret in a machine readable way
        #[clap(short, long)]
        machine: bool
    },
}

#[derive(Subcommand)]
pub enum InfoCommand {
    /// get information about a build
    Build {
        /// id of the build, latest if empty
        id: Option<String>
    },

    /// get logs from a build
    Logs {
        /// id of the build, latest if empty
        id: Option<String>
    },

    /// get the pkgbuild used to build the current package
    Pkgbuild,

    /// set property of the package
    Set {
        /// property to set
        #[clap(subcommand)]
        property: SettingsSubcommand
    }
}

#[derive(Subcommand)]
pub enum SettingsSubcommand {
    /// enable or disable clean build
    Clean {
        /// remove container after build
        #[arg(action = ArgAction::Set)]
        enabled: bool
    },

    /// enable or disable automatic package building
    Enable {
        /// enable automatic building
        #[arg(action = ArgAction::Set)]
        enabled: bool
    },

    /// set custom schedule
    Schedule {
        /// cron string of schedule
        cron: String
    },

    /// set prepare command
    Prepare {
        /// commands to be run before build
        command: String
    }
}