use fluidbg_e2e_tests::harness::E2eHarness;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Kubernetes cluster, Helm, Docker images, and RabbitMQ"]
async fn full_kind_e2e_suite() -> anyhow::Result<()> {
    let mut harness = E2eHarness::setup().await?;
    fluidbg_e2e_tests::scenarios::run_full_suite(&mut harness).await
}
