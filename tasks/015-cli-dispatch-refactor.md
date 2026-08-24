# 015 — Refactor the CLI dispatch block into per-noun helpers

**Source:** code-structure.md § Functions Too Large to Reason About — cli/mod.rs::run; architecture.md § CLI Architecture — critique: fat dispatch block  
**Size:** M  
**Depends on:** nothing (independent; can be done any time)

---

## Problem

`src/cli/mod.rs` is ~977 lines. The `run()` function spans ~290 lines of nested `match` arms. Each arm manually translates CLI argument structs (defined inline in `cli/mod.rs`) into command argument structs (defined in command modules) field by field. Adding a new subcommand requires editing `run()` and the top of `cli/mod.rs` simultaneously.

The file grows with every new command and has no natural stopping point.

## Goal

Factor `run()` into per-noun dispatch helpers (`dispatch_auth`, `dispatch_entry`, `dispatch_list`, etc.). Each helper owns one noun's match arms. Move the CLI arg struct for each command into its command module and add a `From<CliArgs> for CommandArgs` conversion, eliminating the field-by-field translation from `run()`.

## Concrete steps

1. For each top-level noun, create a `dispatch_<noun>` async function in `cli/mod.rs` (or in a new `cli/<noun>.rs` sub-module if the noun is complex):
   ```rust
   async fn dispatch_auth(args: AuthArgs, store: &Store, base_url: &str) -> anyhow::Result<()> {
       match args.command {
           AuthSubcommand::Login(a)  => print_json(&auth_login::execute(store, base_url, a.into()).await?),
           AuthSubcommand::Logout(a) => print_json(&auth_logout::execute(store, a.into())?),
           AuthSubcommand::KeySet(a) => print_json(&auth_key_set::execute(store, a.into())?),
           AuthSubcommand::Whoami(a) => print_json(&auth_whoami::execute(store, base_url, a.into()).await?),
       }
   }
   ```

2. Reduce `run()` to a router:
   ```rust
   pub async fn run() -> anyhow::Result<()> {
       let cli = Cli::parse();
       let store = Store::open_default()?;
       let base_url = resolve_base_url(cli.api_host.as_deref(), &store)?;
       match cli.command {
           TopLevelCommand::Auth(a)         => dispatch_auth(a, &store, &base_url).await,
           TopLevelCommand::Entry(a)        => dispatch_entry(a, &store, &base_url).await,
           TopLevelCommand::Journal(a)      => dispatch_journal(a, &store, &base_url).await,
           TopLevelCommand::List(a)         => dispatch_list(a, &store, &base_url).await,
           TopLevelCommand::Search(a)       => dispatch_search(a, &store, &base_url).await,
           TopLevelCommand::Sync(a)         => dispatch_sync(a, &store, &base_url).await,
           TopLevelCommand::Embeddings(a)   => dispatch_embeddings(a, &store).await,
           TopLevelCommand::DailyChat(a)    => dispatch_daily_chat(a, &store, &base_url).await,
           TopLevelCommand::ContextItem(a)  => dispatch_context_item(a, &store, &base_url).await,
           TopLevelCommand::Profile(a)      => dispatch_profile(a, &store),
           TopLevelCommand::UserSettings(a) => dispatch_user_settings(a, &store, &base_url).await,
           TopLevelCommand::Setup(a)        => dispatch_setup(a, &store, &base_url).await,
       }
   }
   ```

3. For each command module that has a significant gap between its CLI arg struct (in `cli/mod.rs`) and its execute args struct (in the command module), move the CLI arg struct into the command module and add a `From` impl. For small commands where the structs are nearly identical, inline translation is fine.

4. Ensure the existing CLI arg-parsing tests in `cli/mod.rs` still compile and pass.

5. Run `cargo test --locked`.

## Notes

- This is a pure structural refactor — no behaviour change. The goal is that `run()` fits on one screen and each dispatch helper is independently readable.
- There is no need to move the `Cli`, `TopLevelCommand`, and subcommand enum definitions out of `cli/mod.rs` — they can stay there. Only the dispatch logic is being restructured.

## Definition of done

- `run()` in `cli/mod.rs` is under 40 lines.
- Each noun has its own `dispatch_<noun>` function of under 50 lines.
- `cargo test --locked` passes.
- No command behaviour has changed.
