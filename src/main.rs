mod cli;
mod command_error;
mod commands;
mod output;

use clap::Parser;
use cli::{
    AuthCommand, Cli, Command, EditCommand, ObjectCommand, PackageCommand, SystemCommand,
    TableCommand,
};
use commands::auth::{auth_list, auth_login, auth_remove};
use commands::edit_activate::{edit_source_activate, print_edit_source_activate};
use commands::edit_check::{edit_source_check, print_edit_source_check};
use commands::edit_discard::{edit_source_discard, print_edit_source_discard};
use commands::edit_patch::{edit_source_patch, print_edit_source_patch};
use commands::edit_read::{edit_source_read, print_edit_source_read};
use commands::edit_set::{edit_source_set, print_edit_source_set};
use commands::object::{
    object_info, object_kinds, object_search, object_source, object_usages, object_xml,
    print_object_kinds,
};
use commands::package::{package_items, package_tree};
use commands::query::{print_query, query};
use commands::system::{print_system_list, system_list, system_test};
use commands::table::{print_table_data, print_table_metadata, table_data, table_metadata};
use output::{
    default_output_format, run_and_print, run_and_print_async, run_and_print_with,
    run_and_print_with_async,
};

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
        Command::Table {
            command: TableCommand::Data(args),
        } => {
            run_and_print_with_async(
                || table_data(cli.profile.as_deref(), args),
                print_table_data,
                output,
            )
            .await
        }
        Command::Table {
            command: TableCommand::Metadata(args),
        } => {
            run_and_print_with_async(
                || table_metadata(cli.profile.as_deref(), args),
                print_table_metadata,
                output,
            )
            .await
        }
        Command::Query(args) => {
            run_and_print_with_async(|| query(cli.profile.as_deref(), args), print_query, output)
                .await
        }
        Command::Edit {
            command: EditCommand::Read(args),
        } => {
            run_and_print_with_async(
                || edit_source_read(cli.profile.as_deref(), args),
                print_edit_source_read,
                output,
            )
            .await
        }
        Command::Edit {
            command: EditCommand::Patch(args),
        } => {
            run_and_print_with_async(
                || edit_source_patch(cli.profile.as_deref(), args),
                print_edit_source_patch,
                output,
            )
            .await
        }
        Command::Edit {
            command: EditCommand::Set(args),
        } => {
            run_and_print_with_async(
                || edit_source_set(cli.profile.as_deref(), args),
                print_edit_source_set,
                output,
            )
            .await
        }
        Command::Edit {
            command: EditCommand::Check(args),
        } => {
            run_and_print_with_async(
                || edit_source_check(cli.profile.as_deref(), args),
                print_edit_source_check,
                output,
            )
            .await
        }
        Command::Edit {
            command: EditCommand::Activate(args),
        } => {
            run_and_print_with_async(
                || edit_source_activate(cli.profile.as_deref(), args),
                print_edit_source_activate,
                output,
            )
            .await
        }
        Command::Edit {
            command: EditCommand::Discard(args),
        } => {
            run_and_print_with_async(
                || edit_source_discard(cli.profile.as_deref(), args),
                print_edit_source_discard,
                output,
            )
            .await
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
