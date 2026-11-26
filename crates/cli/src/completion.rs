//! `apic completion`: the completion scripts, generated from the command
//! tree.

use clap::{Arg, ArgAction, Command};
use clap_complete::Shell;

use crate::root::{App, CliError, CliResult};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    let no_descriptions = || FlagSpec::bool("no-descriptions", "disable completion descriptions");
    let mut bash = CommandSpec::new("bash", "Generate the autocompletion script for bash")
        .long(
            "Generate the autocompletion script for the bash shell.

This script depends on the 'bash-completion' package.
If it is not installed already, you can install it via your OS's package manager.

To load completions in your current shell session:

	source <(apic completion bash)

To load completions for every new session, execute once:

#### Linux:

	apic completion bash > /etc/bash_completion.d/apic

#### macOS:

	apic completion bash > $(brew --prefix)/etc/bash_completion.d/apic

You will need to start a new shell for this setup to take effect.
",
        )
        .args(ArgRule::None)
        .flag(no_descriptions())
        .runs(|a, _| generate(a, Shell::Bash));
    bash.disable_flags_in_use_line = true;
    let zsh = CommandSpec::new("zsh", "Generate the autocompletion script for zsh")
        .long(
            "Generate the autocompletion script for the zsh shell.

If shell completion is not already enabled in your environment you will need
to enable it.  You can execute the following once:

	echo \"autoload -U compinit; compinit\" >> ~/.zshrc

To load completions in your current shell session:

	source <(apic completion zsh)

To load completions for every new session, execute once:

#### Linux:

	apic completion zsh > \"${fpath[1]}/_apic\"

#### macOS:

	apic completion zsh > $(brew --prefix)/share/zsh/site-functions/_apic

You will need to start a new shell for this setup to take effect.
",
        )
        .args(ArgRule::None)
        .flag(no_descriptions())
        .runs(|a, _| generate(a, Shell::Zsh));
    let fish = CommandSpec::new("fish", "Generate the autocompletion script for fish")
        .long(
            "Generate the autocompletion script for the fish shell.

To load completions in your current shell session:

	apic completion fish | source

To load completions for every new session, execute once:

	apic completion fish > ~/.config/fish/completions/apic.fish

You will need to start a new shell for this setup to take effect.
",
        )
        .args(ArgRule::None)
        .flag(no_descriptions())
        .runs(|a, _| generate(a, Shell::Fish));
    let powershell = CommandSpec::new(
        "powershell",
        "Generate the autocompletion script for powershell",
    )
    .long(
        "Generate the autocompletion script for powershell.

To load completions in your current shell session:

	apic completion powershell | Out-String | Invoke-Expression

To load completions for every new session, add the output of the above command
to your powershell profile.
",
    )
    .args(ArgRule::None)
    .flag(no_descriptions())
    .runs(|a, _| generate(a, Shell::PowerShell));
    let mut cmd = CommandSpec::new(
        "completion",
        "Generate the autocompletion script for the specified shell",
    )
    .long(
        "Generate the autocompletion script for apic for the specified shell.
See each sub-command's help for details on how to use the generated script.
",
    )
    .args(ArgRule::None)
    .subcommand(bash)
    .subcommand(fish)
    .subcommand(powershell)
    .subcommand(zsh);
    cmd.disable_flags_in_use_line = true;
    cmd
}

fn generate(a: &mut App, shell: Shell) -> CliResult {
    let mut cmd = a.command();
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut cmd, "apic", &mut script);
    a.stdout
        .write_text(&String::from_utf8_lossy(&script))
        .map_err(CliError::other)
}

/// The tree as `clap` commands: each command with its own flags, the
/// persistent ones global, and its arguments as one list.
pub(crate) fn clap_tree(spec: &CommandSpec) -> Command {
    let mut cmd = Command::new(spec.name)
        .about(spec.short)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .args_override_self(true)
        .hide(spec.hidden);
    if !spec.long.is_empty() {
        cmd = cmd.long_about(spec.long.clone());
    }
    if !spec.example.is_empty() {
        cmd = cmd.after_help(format!("Examples:\n{}", spec.example));
    }
    for f in &spec.flags {
        cmd = cmd.arg(
            crate::parse::clap_arg(f)
                .help(f.usage.clone())
                .global(f.persistent)
                .hide(f.hidden),
        );
    }
    if spec.args != ArgRule::None {
        let mut arg = Arg::new("args")
            .num_args(0..)
            .action(ArgAction::Append)
            .hide(true);
        if !spec.valid_args.is_empty() {
            arg = arg.value_parser(clap::builder::PossibleValuesParser::new(
                spec.valid_args.clone(),
            ));
        }
        cmd = cmd.arg(arg);
    }
    for sub in &spec.subcommands {
        cmd = cmd.subcommand(clap_tree(sub));
    }
    cmd
}
