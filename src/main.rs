use crate::api::SwClient;
use crate::config::{Credentials, Mapping, Schema};
use crate::data::{export, import, prepare_scripting_environment, ScriptingEnvironment};
use anyhow::Context;
use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod api;
mod config;
mod data;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with a given shopware shop via integration admin API.
    /// Credentials are stored in .credentials.toml in the current working directory.
    Auth {
        /// base URL of the shop
        #[arg(short, long)]
        domain: String,

        /// access_key_id
        #[arg(short, long)]
        id: String,

        /// access_key_secret
        #[arg(short, long)]
        secret: String,
    },

    /// Import data into shopware or export data to a file
    Sync {
        /// Mode (import or export)
        #[arg(value_enum, short, long)]
        mode: SyncMode,

        /// Path to profile schema.yaml
        #[arg(short, long)]
        schema: PathBuf,

        /// Path to data file
        #[arg(short, long)]
        file: PathBuf,

        /// Maximum amount of entities, can be used for debugging
        #[arg(short, long)]
        limit: Option<u64>,

        // Verbose output, used for debugging
        // #[arg(short, long, action = ArgAction::SetTrue)]
        // verbose: bool,
        /// How many requests can be "in-flight" at the same time
        #[arg(short, long, default_value = "8")]
        in_flight_limit: usize,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum SyncMode {
    Import,
    Export,
}

#[derive(Debug)]
pub struct SyncContext {
    pub sw_client: SwClient,
    pub schema: Schema,
    /// specifies the input or output file
    pub file: PathBuf,
    pub limit: Option<u64>,
    pub scripting_environment: ScriptingEnvironment,
    pub associations: HashSet<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start_instant = Instant::now();
    let cli = Cli::parse();

    match cli.command {
        Commands::Auth { domain, id, secret } => {
            auth(domain, id, secret).await?;
            println!("Successfully authenticated. You can continue with other commands now.")
        }
        Commands::Sync {
            mode,
            schema,
            file,
            limit,
            // verbose,
            in_flight_limit,
        } => {
            let context = create_context(schema, file, limit, in_flight_limit).await?;

            match mode {
                SyncMode::Import => {
                    tokio::task::spawn_blocking(|| async move { import(Arc::new(context)).await })
                        .await?
                        .await?;

                    println!("Imported successfully");
                }
                SyncMode::Export => {
                    tokio::task::spawn_blocking(|| async move { export(Arc::new(context)).await })
                        .await?
                        .await?;

                    println!("Exported successfully");
                }
            }
        }
    }

    println!(
        "This whole command executed in {:.3}s",
        start_instant.elapsed().as_secs_f32()
    );

    Ok(())
}

async fn auth(domain: String, id: String, secret: String) -> anyhow::Result<()> {
    let credentials = Credentials {
        base_url: domain.trim_end_matches('/').to_string(),
        access_key_id: id,
        access_key_secret: secret,
    };

    // check if credentials work
    let _ = SwClient::new(credentials.clone(), 8).await?;

    // write them to file
    let serialized = toml::to_string(&credentials)?;
    tokio::fs::write("./.credentials.toml", serialized).await?;

    Ok(())
}

async fn create_context(
    schema: PathBuf,
    file: PathBuf,
    limit: Option<u64>,
    in_flight_limit: usize,
) -> anyhow::Result<SyncContext> {
    let serialized_schema = tokio::fs::read_to_string(schema)
        .await
        .context("No provided schema file not found")?;
    let schema: Schema = serde_yaml::from_str(&serialized_schema)?;
    let mut associations = schema.associations.clone();
    for mapping in &schema.mappings {
        if let Mapping::ByPath(by_path) = mapping {
            if let Some((association, _field)) = by_path.entity_path.rsplit_once('.') {
                associations.insert(association.trim_end_matches('?').to_owned());
            }
        }
    }

    let serialized_credentials = tokio::fs::read_to_string("./.credentials.toml")
        .await
        .context("No .credentials.toml found. Call command auth first.")?;
    let credentials: Credentials = toml::from_str(&serialized_credentials)?;
    let sw_client = SwClient::new(credentials, in_flight_limit).await?;
    // ToDo: lookup entities.json definitions

    // ToDo: further schema verification

    // ToDo: create lookup table for languages + currencies?

    let scripting_environment =
        prepare_scripting_environment(&schema.serialize_script, &schema.deserialize_script)?;

    Ok(SyncContext {
        sw_client,
        schema,
        scripting_environment,
        file,
        limit,
        associations,
    })
}
