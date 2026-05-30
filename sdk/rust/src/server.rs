use anyhow::{Context, Result};
use axum::Router;
use std::sync::Once;

use crate::config::ControlPlaneServerTls;

static RUSTLS_PROVIDER: Once = Once::new();

pub fn install_rustls_crypto_provider() {
    RUSTLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub async fn serve_control_plane(
    app: Router,
    bind_addr: &str,
    tls: ControlPlaneServerTls,
) -> Result<()> {
    let addr: std::net::SocketAddr = bind_addr
        .parse()
        .with_context(|| format!("invalid bind address {bind_addr}"))?;
    if tls.enabled {
        let cert = tls.cert_path.as_deref().context(
            "FLUIDBG_CONTROL_PLANE_TLS_CERT_PATH is required when control-plane TLS is enabled",
        )?;
        let key = tls.key_path.as_deref().context(
            "FLUIDBG_CONTROL_PLANE_TLS_KEY_PATH is required when control-plane TLS is enabled",
        )?;
        install_rustls_crypto_provider();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .with_context(|| {
                format!("failed to load control-plane TLS certificate {cert} and key {key}")
            })?;
        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await?;
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
