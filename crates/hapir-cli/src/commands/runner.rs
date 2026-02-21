pub async fn run(action: Option<&str>) -> anyhow::Result<()> {
    hapir_runner::cli::run(action).await
}
