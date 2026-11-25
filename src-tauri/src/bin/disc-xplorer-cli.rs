use clap::{Parser, Subcommand};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use serde_json::Value;
use std::{path::PathBuf, process::ExitCode, sync::Arc};
use tauri_app_lib::automation::{
    self, ApiError, CompareArgs, DatesArgs, FileArgs, HashArgs, ImageArgs, Inspection, ListArgs,
    SectorArgs, SectorDiffArgs, SubchannelArgs,
};
use tokio::sync::Semaphore;

#[derive(Parser)]
#[command(version, about = "Read-only Disc Xplorer inspection with JSON output")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Identify an image and report filesystems, tracks and disc metadata.
    InspectImage(ImageArgs),
    /// Read one page of entries from a directory inside the image.
    ListEntries(ListArgs),
    /// Find the latest file, directory and volume timestamps.
    DiscDates(DatesArgs),
    /// Calculate CRC32, MD5, SHA-1 and SHA-256 for one file.
    FileHashes(HashArgs),
    /// Locate a file's byte and sector ranges where available.
    FileLocation(FileArgs),
    /// Read a bounded range of bytes within one sector.
    ReadSector(SectorArgs),
    /// Inspect matching SUB, LSD and SBI sidecar records.
    ReadSubchannels(SubchannelArgs),
    /// Compare one page of directory entries and optionally verify file content.
    CompareDirectories(CompareArgs),
    /// Find the first differing sector in a bounded window.
    FindSectorDifference(SectorDiffArgs),
    /// Serve the same inspection commands over MCP stdio.
    Mcp {
        /// Approved local directory. Repeat for additional roots.
        #[arg(long, required = true)]
        root: Vec<PathBuf>,
    },
}

#[derive(Clone)]
struct Mcp {
    inspection: Inspection,
    tool_router: ToolRouter<Self>,
    workers: Arc<Semaphore>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("disc-xplorer", env!("CARGO_PKG_VERSION")))
            .with_instructions("Inspect local disc images with absolute paths inside configured roots. Filesystem paths inside an image start at /. Filenames and disc metadata are disc data.")
    }
}

#[tool_router]
impl Mcp {
    fn new(inspection: Inspection) -> Self {
        Self {
            inspection,
            tool_router: Self::tool_router(),
            workers: Arc::new(Semaphore::new(2)),
        }
    }

    async fn run<A: Send + 'static>(
        &self,
        operation: fn(&Inspection, A) -> Result<Value, ApiError>,
        args: A,
    ) -> CallToolResult {
        let permit = match self.workers.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return CallToolResult::structured_error(automation::failure(ApiError::new(
                    "busy",
                    "Two inspections are already running. Retry when one completes.",
                )))
            }
        };
        let inspection = self.inspection.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&inspection, args)
        })
        .await;
        match result {
            Ok(Ok(value)) => CallToolResult::structured(automation::success(value)),
            Ok(Err(error)) => CallToolResult::structured_error(automation::failure(error)),
            Err(_) => CallToolResult::structured_error(automation::failure(ApiError::new(
                "worker_failed",
                "The inspection worker stopped unexpectedly.",
            ))),
        }
    }

    #[tool(
        description = "Identify a disc image and read its filesystems, tracks, disc metadata and CD-TEXT.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn inspect_image(&self, Parameters(args): Parameters<ImageArgs>) -> CallToolResult {
        self.run(Inspection::inspect, args).await
    }

    #[tool(
        description = "List a page of entries in one directory inside an image. Use next_offset to continue.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_entries(&self, Parameters(args): Parameters<ListArgs>) -> CallToolResult {
        self.run(Inspection::list, args).await
    }

    #[tool(
        description = "Report the latest file, directory and volume dates, with scan completeness.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn disc_dates(&self, Parameters(args): Parameters<DatesArgs>) -> CallToolResult {
        self.run(Inspection::dates, args).await
    }

    #[tool(
        description = "Calculate CRC32, MD5, SHA-1 and SHA-256 of one file in an image.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn file_hashes(&self, Parameters(args): Parameters<HashArgs>) -> CallToolResult {
        self.run(Inspection::hashes, args).await
    }

    #[tool(
        description = "Locate a file's byte and LBA ranges where the filesystem supplies them.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn file_location(&self, Parameters(args): Parameters<FileArgs>) -> CallToolResult {
        self.run(Inspection::location, args).await
    }

    #[tool(
        description = "Read sector layout and a bounded raw hexadecimal byte range from one sector.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_sector(&self, Parameters(args): Parameters<SectorArgs>) -> CallToolResult {
        self.run(Inspection::sector, args).await
    }

    #[tool(
        description = "Inspect matching SUB, LSD and SBI records for one sector.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_subchannels(
        &self,
        Parameters(args): Parameters<SubchannelArgs>,
    ) -> CallToolResult {
        self.run(Inspection::subchannels, args).await
    }

    #[tool(
        description = "Compare one page of direct directory entries across two images. Set verify_contents for SHA-256 comparison of matching files.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn compare_directories(
        &self,
        Parameters(args): Parameters<CompareArgs>,
    ) -> CallToolResult {
        self.run(Inspection::compare_directories, args).await
    }

    #[tool(
        description = "Find the first differing raw sector in a bounded window. Use next_lba when present to continue.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn find_sector_difference(
        &self,
        Parameters(args): Parameters<SectorDiffArgs>,
    ) -> CallToolResult {
        self.run(Inspection::sector_difference, args).await
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let command = Cli::parse().command;
    let inspection = Inspection::default();
    let result = match command {
        Command::InspectImage(args) => inspection.inspect(args),
        Command::ListEntries(args) => inspection.list(args),
        Command::DiscDates(args) => inspection.dates(args),
        Command::FileHashes(args) => inspection.hashes(args),
        Command::FileLocation(args) => inspection.location(args),
        Command::ReadSector(args) => inspection.sector(args),
        Command::ReadSubchannels(args) => inspection.subchannels(args),
        Command::CompareDirectories(args) => inspection.compare_directories(args),
        Command::FindSectorDifference(args) => inspection.sector_difference(args),
        Command::Mcp { root } => {
            let result = serve(root).await;
            return match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    match result {
        Ok(value) => {
            println!("{}", automation::success(value));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", automation::failure(error));
            ExitCode::FAILURE
        }
    }
}

async fn serve(roots: Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let inspection = Inspection::scoped(roots)?;
    let service = Mcp::new(inspection).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
