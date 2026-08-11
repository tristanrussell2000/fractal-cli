mod cli;
mod command_error;
mod commands;
mod output;

use clap::Parser;
use cli::{AuthCommand, Cli, Command, ObjectCommand, PackageCommand, SystemCommand};
use commands::auth::{auth_list, auth_login, auth_remove};
use commands::object::{
    object_info, object_kinds, object_search, object_source, object_usages, object_xml,
    print_object_kinds,
};
use commands::package::{package_items, package_tree};
use commands::system::{print_system_list, system_list, system_test};
use output::{default_output_format, run_and_print, run_and_print_async, run_and_print_with};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let output = cli.output.unwrap_or_else(default_output_format);

    let exit_code = match &cli.command {
        Command::Auth {
            command: AuthCommand::Login(args),
        } => run_and_print(|| auth_login(args), output),
        Command::Auth {
            command: AuthCommand::List,
        } => run_and_print(auth_list, output),
        Command::Auth {
            command: AuthCommand::Remove(args),
        } => run_and_print(|| auth_remove(args), output),
        Command::System {
            command: SystemCommand::List,
        } => run_and_print_with(system_list, print_system_list, output),
        Command::System {
            command: SystemCommand::Test,
        } => run_and_print_async(|| system_test(cli.profile.as_deref()), output).await,
        Command::Object {
            command: ObjectCommand::Search(args),
        } => run_and_print_async(|| object_search(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Source(args),
        } => run_and_print_async(|| object_source(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Xml(args),
        } => run_and_print_async(|| object_xml(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Info(args),
        } => run_and_print_async(|| object_info(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Usages(args),
        } => run_and_print_async(|| object_usages(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Kinds,
        } => run_and_print_with(object_kinds, print_object_kinds, output),
        Command::Package {
            command: PackageCommand::Tree(args),
        } => run_and_print_async(|| package_tree(cli.profile.as_deref(), args), output).await,
        Command::Package {
            command: PackageCommand::Items(args),
        } => run_and_print_async(|| package_items(cli.profile.as_deref(), args), output).await,
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
