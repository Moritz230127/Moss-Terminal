use anyhow::Result;
use moss::{cli, config, i18n, paths};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}: {error:#}", i18n::text("error", "错误"));
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let paths = paths::MossPaths::new()?;
    let language = config::AppConfig::display_language_hint(&paths);
    i18n::init(language.as_deref().unwrap_or("auto"));
    let cli = cli::parse();
    cli::run(cli, paths).await
}
