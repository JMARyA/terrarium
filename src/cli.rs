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
}

#[derive(FromArgs)]
/// Start the server
#[argh(subcommand, name = "serve")]
pub struct ServeCommand {}

#[derive(FromArgs)]
#[argh(subcommand, name = "user")]
/// user commands
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
