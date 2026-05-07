use argh::FromArgs;

#[derive(FromArgs)]
/// terra CLI - A unified Terraform/OpenTofu framework
pub struct Cli {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum SubCommand {
    // ── Tofu workflow commands ──
    Init(InitCommand),
    Validate(ValidateCommand),
    Plan(PlanCommand),
    Apply(ApplyCommand),
    Destroy(DestroyCommand),

    // ── Tofu utility commands ──
    Console(ConsoleCommand),
    Fmt(FmtCommand),
    ForceUnlock(ForceUnlockCommand),
    Get(GetCommand),
    Graph(GraphCommand),
    Import(ImportCommand),
    Login(LoginCommand),
    Logout(LogoutCommand),
    Output(OutputCommand),
    Providers(ProvidersCommand),
    Refresh(RefreshCommand),
    Show(ShowCommand),
    Taint(TaintCommand),
    Untaint(UntaintCommand),
    Test(TestCommand),
    Version(VersionCommand),
    Workspace(WorkspaceCommand),
    State(StateCommand),
    Metadata(MetadataCommand),

    // ── Native terrarium commands ──
    Serve(ServeCommand),
    User(UserCommand),
    Remote(RemoteCommand),
    TerrariumLogin(TerrariumLoginCommand),

    // ── Terranix commands ──
    Nix(NixCommand),
}

// ── Tofu workflow commands ────────────────────────────────────────────────

#[derive(FromArgs)]
/// Initialize a Terraform/OpenTofu working directory
#[argh(subcommand, name = "init")]
pub struct InitCommand {
    /// path to copy module from
    #[argh(option)]
    pub from_module: Option<String>,
    /// reconfigure the backend
    #[argh(switch)]
    pub reconfigure: bool,
    /// migrate state from existing backend
    #[argh(switch)]
    pub migrate_state: bool,
    /// backend configuration variable (key=value)
    #[argh(option)]
    pub backend_config: Vec<String>,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// upgrade modules and plugins
    #[argh(switch)]
    pub upgrade: bool,
    /// get plugins only from specified directory
    #[argh(option)]
    pub plugin_dir: Option<String>,
    /// lockfile mode
    #[argh(option)]
    pub lockfile: Option<String>,
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
}

#[derive(FromArgs)]
/// Validate the configuration
#[argh(subcommand, name = "validate")]
pub struct ValidateCommand {
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
/// Show changes required by the current configuration
#[argh(subcommand, name = "plan")]
pub struct PlanCommand {
    /// output plan to file
    #[argh(option)]
    pub out: Option<String>,
    /// destroy all resources
    #[argh(switch)]
    pub destroy: bool,
    /// refresh-only plan
    #[argh(switch)]
    pub refresh_only: bool,
    /// replace specific resource
    #[argh(option)]
    pub replace: Vec<String>,
    /// target specific resource
    #[argh(option)]
    pub target: Vec<String>,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// parallelism
    #[argh(option)]
    pub parallelism: Option<u32>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// no color in output
    #[argh(switch)]
    pub no_color: bool,
    /// skip pre-plan refresh of state
    #[argh(switch)]
    pub no_refresh: bool,
}

#[derive(FromArgs)]
/// Create or update infrastructure
#[argh(subcommand, name = "apply")]
pub struct ApplyCommand {
    /// auto approve without prompt
    #[argh(switch)]
    pub auto_approve: bool,
    /// plan file to apply
    #[argh(positional)]
    pub plan: Option<String>,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// parallelism
    #[argh(option)]
    pub parallelism: Option<u32>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// no color in output
    #[argh(switch)]
    pub no_color: bool,
    /// replace specific resource
    #[argh(option)]
    pub replace: Vec<String>,
    /// destroy all resources
    #[argh(switch)]
    pub destroy: bool,
    /// refresh-only apply
    #[argh(switch)]
    pub refresh_only: bool,
}

#[derive(FromArgs)]
/// Destroy previously-created infrastructure
#[argh(subcommand, name = "destroy")]
pub struct DestroyCommand {
    /// auto approve without prompt
    #[argh(switch)]
    pub auto_approve: bool,
    /// target specific resource
    #[argh(option)]
    pub target: Vec<String>,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// parallelism
    #[argh(option)]
    pub parallelism: Option<u32>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
    /// no color in output
    #[argh(switch)]
    pub no_color: bool,
}

// ── Tofu utility commands ──────────────────────────────────────────────────

#[derive(FromArgs)]
/// Try OpenTofu expressions at an interactive command prompt
#[argh(subcommand, name = "console")]
pub struct ConsoleCommand {}

#[derive(FromArgs)]
/// Reformat your configuration in the standard style
#[argh(subcommand, name = "fmt")]
pub struct FmtCommand {
    /// check if files are formatted
    #[argh(switch)]
    pub check: bool,
    /// recursively format directories
    #[argh(switch)]
    pub recursive: bool,
    /// diff the changes
    #[argh(switch)]
    pub diff: bool,
    /// write stdout compatible output
    #[argh(switch)]
    pub stdio: bool,
    /// list files that would be formatted
    #[argh(switch)]
    pub list: bool,
    /// no color in output
    #[argh(switch)]
    pub no_color: bool,
    /// paths to format
    #[argh(positional)]
    pub paths: Vec<String>,
}

#[derive(FromArgs)]
/// Release a stuck lock on the current workspace
#[argh(subcommand, name = "force-unlock")]
pub struct ForceUnlockCommand {
    /// don't ask for confirmation
    #[argh(switch)]
    pub force: bool,
    /// lock ID to unlock
    #[argh(positional)]
    pub lock_id: String,
}

#[derive(FromArgs)]
/// Install or upgrade remote OpenTofu modules
#[argh(subcommand, name = "get")]
pub struct GetCommand {
    /// update modules even if already present
    #[argh(switch)]
    pub update: bool,
}

#[derive(FromArgs)]
/// Generate a Graphviz graph of the steps in an operation
#[argh(subcommand, name = "graph")]
pub struct GraphCommand {
    /// output type (dot)
    #[argh(option)]
    pub type_: Option<String>,
}

#[derive(FromArgs)]
/// Associate existing infrastructure with a OpenTofu resource
#[argh(subcommand, name = "import")]
pub struct ImportCommand {
    /// resource address
    #[argh(positional)]
    pub address: String,
    /// resource ID
    #[argh(positional)]
    pub id: String,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
}

#[derive(FromArgs)]
/// Obtain and save credentials for a remote host
#[argh(subcommand, name = "login")]
pub struct LoginCommand {
    /// hostname to login to
    #[argh(positional)]
    pub hostname: Option<String>,
}

#[derive(FromArgs)]
/// Remove locally-stored credentials for a remote host
#[argh(subcommand, name = "logout")]
pub struct LogoutCommand {
    /// hostname to logout from
    #[argh(positional)]
    pub hostname: Option<String>,
}

#[derive(FromArgs)]
/// Show output values from your root module
#[argh(subcommand, name = "output")]
pub struct OutputCommand {
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// show raw output value
    #[argh(switch)]
    pub raw: bool,
    /// specific output name
    #[argh(positional)]
    pub name: Option<String>,
}

#[derive(FromArgs)]
/// Show the providers required for this configuration
#[argh(subcommand, name = "providers")]
pub struct ProvidersCommand {
    /// show mirror instructions
    #[argh(switch)]
    pub mirror: bool,
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
/// Update the state to match remote systems
#[argh(subcommand, name = "refresh")]
pub struct RefreshCommand {
    /// target specific resource
    #[argh(option)]
    pub target: Vec<String>,
    /// disable interactive input prompts
    #[argh(switch)]
    pub no_input: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
}

#[derive(FromArgs)]
/// Show the current state or a saved plan
#[argh(subcommand, name = "show")]
pub struct ShowCommand {
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// show sensitive values
    #[argh(switch)]
    pub show_sensitive: bool,
    /// plan file to show
    #[argh(positional)]
    pub plan: Option<String>,
}

#[derive(FromArgs)]
/// Mark a resource instance as not fully functional
#[argh(subcommand, name = "taint")]
pub struct TaintCommand {
    /// resource address to taint
    #[argh(positional)]
    pub address: String,
    /// allow missing resource
    #[argh(switch)]
    pub allow_missing: bool,
}

#[derive(FromArgs)]
/// Remove the 'tainted' state from a resource instance
#[argh(subcommand, name = "untaint")]
pub struct UntaintCommand {
    /// resource address to untaint
    #[argh(positional)]
    pub address: String,
    /// allow missing resource
    #[argh(switch)]
    pub allow_missing: bool,
}

#[derive(FromArgs)]
/// Run infrastructure tests
#[argh(subcommand, name = "test")]
pub struct TestCommand {
    /// filter to specific test files
    #[argh(option)]
    pub filter: Vec<String>,
    /// output in JSON format
    #[argh(switch)]
    pub json: bool,
    /// no color in output
    #[argh(switch)]
    pub no_color: bool,
}

#[derive(FromArgs)]
/// Show the current OpenTofu version
#[argh(subcommand, name = "version")]
pub struct VersionCommand {}

#[derive(FromArgs)]
/// Workspace management
#[argh(subcommand, name = "workspace")]
pub struct WorkspaceCommand {
    #[argh(subcommand)]
    pub subcommand: WorkspaceSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum WorkspaceSubCommand {
    List(WorkspaceList),
    Show(WorkspaceShow),
    New(WorkspaceNew),
    Select(WorkspaceSelect),
    Delete(WorkspaceDelete),
}

#[derive(FromArgs)]
/// List workspaces
#[argh(subcommand, name = "list")]
pub struct WorkspaceList {}

#[derive(FromArgs)]
/// Show current workspace
#[argh(subcommand, name = "show")]
pub struct WorkspaceShow {}

#[derive(FromArgs)]
/// Create a new workspace
#[argh(subcommand, name = "new")]
pub struct WorkspaceNew {
    /// workspace name
    #[argh(positional)]
    pub name: String,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Select a workspace
#[argh(subcommand, name = "select")]
pub struct WorkspaceSelect {
    /// workspace name
    #[argh(positional)]
    pub name: String,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Delete a workspace
#[argh(subcommand, name = "delete")]
pub struct WorkspaceDelete {
    /// workspace name
    #[argh(positional)]
    pub name: String,
    /// force delete
    #[argh(switch)]
    pub force: bool,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Advanced state management
#[argh(subcommand, name = "state")]
pub struct StateCommand {
    #[argh(subcommand)]
    pub subcommand: StateSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum StateSubCommand {
    List(StateList),
    Show(StateShow),
    MV(StateMV),
    RM(StateRM),
    Pull(StatePull),
    Push(StatePush),
    ReplaceProvider(StateReplaceProvider),
}

#[derive(FromArgs)]
/// List resources in state
#[argh(subcommand, name = "list")]
pub struct StateList {
    /// resource address to filter
    #[argh(positional)]
    pub addresses: Vec<String>,
    /// specific state file
    #[argh(option)]
    pub state: Option<String>,
    /// resource ID to filter
    #[argh(option)]
    pub id: Option<String>,
    /// set a variable (key=value)
    #[argh(option)]
    pub var: Vec<String>,
    /// set variables from a file
    #[argh(option)]
    pub var_file: Vec<String>,
}

#[derive(FromArgs)]
/// Show a resource in state
#[argh(subcommand, name = "show")]
pub struct StateShow {
    /// resource address
    #[argh(positional)]
    pub address: String,
    /// specific state file
    #[argh(option)]
    pub state: Option<String>,
}

#[derive(FromArgs)]
/// Move a resource in state
#[argh(subcommand, name = "mv")]
pub struct StateMV {
    /// source address
    #[argh(positional)]
    pub source: String,
    /// destination address
    #[argh(positional)]
    pub destination: String,
    /// specific state file
    #[argh(option)]
    pub state: Option<String>,
    /// state output file
    #[argh(option)]
    pub state_out: Option<String>,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Remove a resource from state
#[argh(subcommand, name = "rm")]
pub struct StateRM {
    /// resource addresses to remove
    #[argh(positional)]
    pub addresses: Vec<String>,
    /// specific state file
    #[argh(option)]
    pub state: Option<String>,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Pull state from backend
#[argh(subcommand, name = "pull")]
pub struct StatePull {}

#[derive(FromArgs)]
/// Push state to backend
#[argh(subcommand, name = "push")]
pub struct StatePush {}

#[derive(FromArgs)]
/// Replace provider in state
#[argh(subcommand, name = "replace-provider")]
pub struct StateReplaceProvider {
    /// old provider address
    #[argh(positional)]
    pub old_provider: String,
    /// new provider address
    #[argh(positional)]
    pub new_provider: String,
    /// mirror state
    #[argh(option)]
    pub mirror_state: Option<String>,
    /// disable state locking
    #[argh(switch)]
    pub no_lock: bool,
    /// lock timeout duration
    #[argh(option)]
    pub lock_timeout: Option<String>,
}

#[derive(FromArgs)]
/// Metadata related commands
#[argh(subcommand, name = "metadata")]
pub struct MetadataCommand {
    #[argh(subcommand)]
    pub subcommand: MetadataSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum MetadataSubCommand {
    // Add specific metadata subcommands as needed
}

// ── Terranix commands ──────────────────────────────────────────────────────

#[derive(FromArgs)]
/// Generate Terraform/OpenTofu JSON from a Nix config using terranix
#[argh(subcommand, name = "nix")]
pub struct NixCommand {
    #[argh(subcommand)]
    pub subcommand: NixSubCommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub enum NixSubCommand {
    Generate(GenerateCommand),
}

#[derive(FromArgs)]
/// Generate config.tf.json from a .nix file using terranix
#[argh(subcommand, name = "generate")]
pub struct GenerateCommand {
    /// path to the nix config file
    #[argh(positional)]
    pub config: String,
    /// output file path (defaults to stdout)
    #[argh(option, short = 'o')]
    pub output: Option<String>,
    /// extra arguments passed to terranix
    #[argh(option)]
    pub arg: Vec<String>,
    /// extra string arguments passed to terranix
    #[argh(option)]
    pub argstr: Vec<String>,
}

// ── Native terrarium commands ──────────────────────────────────────────────

#[derive(FromArgs)]
/// Save credentials for a terrarium server
#[argh(subcommand, name = "terra-login")]
pub struct TerrariumLoginCommand {
    /// path to config file (overrides TERRARIUM_CONFIG env var)
    #[argh(option)]
    pub config: Option<String>,
}

#[derive(FromArgs)]
/// Start the terra server
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

// ── Remote client commands (native terrarium) ─────────────────────────────

#[derive(FromArgs)]
/// Interact with a remote terra server
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
