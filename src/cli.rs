use argh::FromArgs;

#[derive(FromArgs)]
/// terrarium CLI
pub struct Cli {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum SubCommand {
    Serve(ServeCommand),
    User(UserCommand),
    Remote(RemoteCommand),
    Login(LoginCommand),
}

// ── Local server commands ────────────────────────────────────────────────────

#[derive(FromArgs)]
/// Start the server
#[argh(subcommand, name = "serve")]
pub struct ServeCommand {}

#[derive(FromArgs)]
#[argh(subcommand, name = "user")]
/// Manage local user database (server-side admin)
pub struct UserCommand {
    #[argh(subcommand)]
    pub subcommand: UserCommands,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum UserCommands {
    Add(AddUser),
    Delete(DeleteUser),
    ChangePassword(ChangePassword),
    List(ListUsers),
}

#[derive(FromArgs)]
/// Add a new user
#[argh(subcommand, name = "add")]
pub struct AddUser {
    /// username
    #[argh(positional)]
    pub username: String,

    /// password
    #[argh(positional)]
    pub password: Option<String>,
}

#[derive(FromArgs)]
/// Delete a user
#[argh(subcommand, name = "delete")]
pub struct DeleteUser {
    /// username
    #[argh(positional)]
    pub username: String,
}

#[derive(FromArgs)]
/// Change a user's password
#[argh(subcommand, name = "passwd")]
pub struct ChangePassword {
    /// username
    #[argh(positional)]
    pub username: String,

    /// new password
    #[argh(positional)]
    pub password: Option<String>,
}

#[derive(FromArgs)]
/// List all users
#[argh(subcommand, name = "list")]
pub struct ListUsers {}

// ── Client setup ─────────────────────────────────────────────────────────────

#[derive(FromArgs)]
/// Save server URL, username, and password to the local config file (chmod 600)
#[argh(subcommand, name = "login")]
pub struct LoginCommand {
    /// config file to write (default: ~/.config/terrarium/config.toml)
    #[argh(option)]
    pub config: Option<String>,
}

// ── Remote client commands ───────────────────────────────────────────────────

#[derive(FromArgs)]
/// Interact with a remote terrarium server
#[argh(subcommand, name = "remote")]
pub struct RemoteCommand {
    /// path to config file (overrides TERRARIUM_CONFIG env var)
    #[argh(option)]
    pub config: Option<String>,

    #[argh(subcommand)]
    pub subcommand: RemoteSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum RemoteSubCommand {
    State(RemoteStateCommand),
    Lock(RemoteLockCommand),
    User(RemoteUserCommand),
    Webhook(RemoteWebhookCommand),
}

// state

#[derive(FromArgs)]
/// Manage terraform states on the server
#[argh(subcommand, name = "state")]
pub struct RemoteStateCommand {
    #[argh(subcommand)]
    pub subcommand: RemoteStateSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum RemoteStateSubCommand {
    List(RemoteStateList),
    Get(RemoteStateGet),
    Versions(RemoteStateVersions),
    Diff(RemoteStateDiff),
    Unlock(RemoteStateUnlock),
    Archive(RemoteStateArchive),
    Unarchive(RemoteStateUnarchive),
}

#[derive(FromArgs)]
/// List all states, optionally scoped to a path prefix (e.g. "infra/")
#[argh(subcommand, name = "list")]
pub struct RemoteStateList {
    /// path prefix to filter by (e.g. infra/)
    #[argh(positional)]
    pub prefix: Option<String>,

    /// show only archived states instead of active ones
    #[argh(switch)]
    pub archived: bool,
}

#[derive(FromArgs)]
/// Get a state's content (pretty-printed by default)
#[argh(subcommand, name = "get")]
pub struct RemoteStateGet {
    /// state name
    #[argh(positional)]
    pub name: String,

    /// output raw JSON without formatting
    #[argh(switch)]
    pub raw: bool,

    /// retrieve a specific version number instead of the current state
    #[argh(option)]
    pub version: Option<u32>,
}

#[derive(FromArgs)]
/// List all available versions for a state
#[argh(subcommand, name = "versions")]
pub struct RemoteStateVersions {
    /// state name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
/// Diff two versions of a state
#[argh(subcommand, name = "diff")]
pub struct RemoteStateDiff {
    /// state name
    #[argh(positional)]
    pub name: String,

    /// from version number
    #[argh(positional)]
    pub from: u32,

    /// to version number
    #[argh(positional)]
    pub to: u32,
}

#[derive(FromArgs)]
/// Force-unlock a state
#[argh(subcommand, name = "unlock")]
pub struct RemoteStateUnlock {
    /// state name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
/// Archive a state — marks it read-only, rejects future pushes
#[argh(subcommand, name = "archive")]
pub struct RemoteStateArchive {
    /// state name (e.g. infra/prod)
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
/// Unarchive a state — re-enables writes
#[argh(subcommand, name = "unarchive")]
pub struct RemoteStateUnarchive {
    /// state name (e.g. infra/prod)
    #[argh(positional)]
    pub name: String,
}

// lock

#[derive(FromArgs)]
/// Inspect active locks on the server
#[argh(subcommand, name = "lock")]
pub struct RemoteLockCommand {
    #[argh(subcommand)]
    pub subcommand: RemoteLockSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum RemoteLockSubCommand {
    List(RemoteLockList),
}

#[derive(FromArgs)]
/// List all active locks
#[argh(subcommand, name = "list")]
pub struct RemoteLockList {}

// user (self-service)

#[derive(FromArgs)]
/// Self-service user commands (acts on your own account via the API)
#[argh(subcommand, name = "user")]
pub struct RemoteUserCommand {
    #[argh(subcommand)]
    pub subcommand: RemoteUserSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum RemoteUserSubCommand {
    Passwd(RemoteUserPasswd),
}

#[derive(FromArgs)]
/// Change your own password
#[argh(subcommand, name = "passwd")]
pub struct RemoteUserPasswd {
    /// new password (will prompt if not provided)
    #[argh(positional)]
    pub password: Option<String>,
}

// webhook

#[derive(FromArgs)]
/// Manage webhooks for a workspace
#[argh(subcommand, name = "webhook")]
pub struct RemoteWebhookCommand {
    #[argh(subcommand)]
    pub subcommand: RemoteWebhookSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum RemoteWebhookSubCommand {
    Add(RemoteWebhookAdd),
    List(RemoteWebhookList),
    Remove(RemoteWebhookRemove),
}

#[derive(FromArgs)]
/// Register a webhook for a workspace
#[argh(subcommand, name = "add")]
pub struct RemoteWebhookAdd {
    /// workspace name (e.g. infra/prod)
    #[argh(positional)]
    pub workspace: String,

    /// webhook URL to POST events to
    #[argh(positional)]
    pub url: String,

    /// comma-separated events to subscribe to (default: all)
    /// e.g. state.push,lock.acquire
    #[argh(option)]
    pub events: Option<String>,
}

#[derive(FromArgs)]
/// List webhooks for a workspace
#[argh(subcommand, name = "list")]
pub struct RemoteWebhookList {
    /// workspace name
    #[argh(positional)]
    pub workspace: String,
}

#[derive(FromArgs)]
/// Remove a webhook by ID
#[argh(subcommand, name = "remove")]
pub struct RemoteWebhookRemove {
    /// webhook ID (from webhook list)
    #[argh(positional)]
    pub id: String,
}
