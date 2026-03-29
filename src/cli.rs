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
    Unlock(RemoteStateUnlock),
    Archive(RemoteStateArchive),
}

#[derive(FromArgs)]
/// List all states, optionally scoped to a path prefix (e.g. "infra/")
#[argh(subcommand, name = "list")]
pub struct RemoteStateList {
    /// path prefix to filter by (e.g. infra/)
    #[argh(positional)]
    pub prefix: Option<String>,
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
