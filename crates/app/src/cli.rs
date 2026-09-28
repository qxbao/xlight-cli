// SPDX-License-Identifier: GPL-3.0-only

//! Clap CLI surface (docs/PLAN.md §18.3). Phase 0 implemented `dev probe`, `auth
//! {list,login,logout,import}`, and `provider {list,info}`. Phase 1 Wave A adds the `exec`
//! headless surface (D-026, docs/commands.md §5) and an `init` stub; `config` and a real bare TUI
//! (today: a thin wrapper around `xlightcli_tui::run`) round out docs/PLAN.md §18.3.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "xlightcli",
    version,
    about = "xlightcli — lightweight multi-agent coding terminal"
)]
pub struct Cli {
    /// Print debug-level logs to stderr in addition to the redacted log file (PATTERNS.md §14).
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Developer-only commands, not part of the stable CLI surface.
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
    /// Credential management (docs/PLAN.md §6).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Provider/transport introspection.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Headless prompt execution (docs/PLAN.md §9.3, §18.3; docs/commands.md §5; D-026).
    Exec(ExecArgs),
    /// Interactive setup wizard (docs/PLAN.md §18.1). Stub — Phase 1 Wave B.
    Init,
}

/// `--output-format` (docs/commands.md §5), mapped to `xlightcli_runtime::ExecOutputFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExecOutputFormatArg {
    Text,
    Json,
    StreamJson,
}

impl From<ExecOutputFormatArg> for xlightcli_runtime::ExecOutputFormat {
    fn from(value: ExecOutputFormatArg) -> Self {
        match value {
            ExecOutputFormatArg::Text => Self::Text,
            ExecOutputFormatArg::Json => Self::Json,
            ExecOutputFormatArg::StreamJson => Self::StreamJson,
        }
    }
}

/// `xlightcli exec` flags (D-026, docs/commands.md §5 — kept compatible with agy's/Claude Code's
/// print-mode flags). Exactly one of `--print`/the positional `prompt` must be given; that's
/// validated in `cmd::exec::dispatch` rather than with clap groups, so the error message can be a
/// normal `CliError::invalid_input` (exit code 2, docs/PLAN.md §18.3) instead of clap's own usage
/// error.
#[derive(Debug, Clone, clap::Args)]
pub struct ExecArgs {
    /// `-p`/`--print "<prompt>"` (agy, Claude Code compatibility).
    #[arg(short = 'p', long = "print")]
    pub print: Option<String>,

    /// Positional prompt, used when `--print` isn't given.
    pub prompt: Option<String>,

    #[arg(long = "output-format", value_enum, default_value_t = ExecOutputFormatArg::Text)]
    pub output_format: ExecOutputFormatArg,

    #[arg(long)]
    pub model: Option<String>,

    #[arg(long)]
    pub provider: Option<String>,

    #[arg(long)]
    pub transport: Option<String>,

    /// Execution mode: `default` | `accept-edits` | `plan`.
    #[arg(long)]
    pub mode: Option<String>,

    /// `-c`/`--continue`: continue the most recent session in this workspace.
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// `--resume <id>`.
    #[arg(long)]
    pub resume: Option<String>,

    /// Requires workspace trust + a global opt-in (docs/commands.md §5); enforcing that is Wave B.
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// `--add-dir`, repeatable.
    #[arg(long)]
    pub add_dir: Vec<PathBuf>,

    /// `--print-timeout`, seconds (default 5 minutes, docs/commands.md §5).
    #[arg(long)]
    pub print_timeout: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Sends a single prompt to a provider/transport and prints the streamed response
    /// (docs/PLAN.md §19, Phase 0 exit criteria).
    Probe {
        /// Provider id: "codex" | "claude" | "agy".
        provider: String,
        /// Transport id (defaults to the provider's stable transport).
        #[arg(long)]
        transport: Option<String>,
        /// Model id (defaults to the first model the transport lists).
        #[arg(long)]
        model: Option<String>,
        /// Prompt text, sent as the single user turn (`TurnRequest::simple`).
        prompt: String,
    },
    /// Lists the models a provider/transport offers to the logged-in account.
    Models {
        provider: String,
        #[arg(long)]
        transport: Option<String>,
    },
    /// Shows plan/quota information from the provider, when the transport exposes it.
    Quota {
        provider: String,
        #[arg(long)]
        transport: Option<String>,
    },
}

/// CLI-facing login method selector; mapped to `xlightcli_auth::AuthMethod` in `cmd::auth`
/// (kept separate so the CLI surface doesn't need to change if the auth-side enum grows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoginMethodArg {
    Browser,
    Device,
    ApiKey,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Lists accounts stored by xlightcli, plus credentials discoverable from other CLIs' config
    /// that can be imported (read-only scan, D-017).
    List,
    /// Logs into `provider` (browser OAuth, device code, or API key). Without `--method`, the
    /// method follows the transport (API-key transports prompt for a key, subscription transports
    /// open the browser); without `--transport`, the first transport matching `--method` is used.
    Login {
        provider: String,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long, value_enum)]
        method: Option<LoginMethodArg>,
    },
    /// Logs out of `provider` (revokes + removes the stored credential).
    Logout {
        provider: String,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long)]
        account: Option<String>,
    },
    /// Imports an existing credential discovered from another CLI's config (D-017: import & own).
    Import { provider: String },
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    /// Lists every registered provider and its transports.
    List,
    /// Shows detailed capability/transport/command info for one provider.
    Info { provider: String },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use clap::error::ErrorKind;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("xlightcli").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn no_subcommand_parses_to_none() {
        let cli = parse(&[]);
        assert!(cli.command.is_none());
        assert!(!cli.verbose);
    }

    #[test]
    fn verbose_flag_is_global() {
        let cli = parse(&["-v", "provider", "list"]);
        assert!(cli.verbose);
        assert!(matches!(
            cli.command,
            Some(Command::Provider {
                command: ProviderCommand::List
            })
        ));
    }

    #[test]
    fn dev_probe_parses_all_fields() {
        let cli = parse(&[
            "dev",
            "probe",
            "codex",
            "--transport",
            "chatgpt",
            "--model",
            "gpt-5",
            "hello",
        ]);
        match cli.command {
            Some(Command::Dev {
                command:
                    DevCommand::Probe {
                        provider,
                        transport,
                        model,
                        prompt,
                    },
            }) => {
                assert_eq!(provider, "codex");
                assert_eq!(transport.as_deref(), Some("chatgpt"));
                assert_eq!(model.as_deref(), Some("gpt-5"));
                assert_eq!(prompt, "hello");
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn dev_probe_requires_a_prompt() {
        let err = Cli::try_parse_from(["xlightcli", "dev", "probe", "codex"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn auth_login_method_is_optional() {
        let cli = parse(&["auth", "login", "claude"]);
        match cli.command {
            Some(Command::Auth {
                command:
                    AuthCommand::Login {
                        provider,
                        transport,
                        method,
                    },
            }) => {
                assert_eq!(provider, "claude");
                assert_eq!(transport, None);
                assert_eq!(method, None);
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn auth_login_accepts_explicit_method_and_transport() {
        let cli = parse(&[
            "auth",
            "login",
            "agy",
            "--transport",
            "antigravity",
            "--method",
            "device",
        ]);
        match cli.command {
            Some(Command::Auth {
                command:
                    AuthCommand::Login {
                        transport, method, ..
                    },
            }) => {
                assert_eq!(transport.as_deref(), Some("antigravity"));
                assert_eq!(method, Some(LoginMethodArg::Device));
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn auth_login_rejects_unknown_method() {
        let err = Cli::try_parse_from([
            "xlightcli",
            "auth",
            "login",
            "codex",
            "--method",
            "carrier-pigeon",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn provider_info_requires_provider_arg() {
        let err = Cli::try_parse_from(["xlightcli", "provider", "info"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn unknown_top_level_subcommand_is_rejected() {
        let err = Cli::try_parse_from(["xlightcli", "not-a-command"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }
}
