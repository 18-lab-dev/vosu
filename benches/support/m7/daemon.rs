pub use vosu_core::runtime::TestDaemon;

pub async fn new(root: &std::path::Path) -> TestDaemon {
    TestDaemon::new(root, &vosu::BUNDLE).await
}
