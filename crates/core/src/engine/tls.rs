//! 入站 TLS 配置：从 PEM 文件构建 rustls ServerConfig。

use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use tokio_rustls::TlsAcceptor;

use crate::config::TlsConfig;
use crate::error::{Error, Result};

/// 从配置中的 PEM 证书与私钥文件构建 [`TlsAcceptor`]。
pub fn build_acceptor(cfg: &TlsConfig) -> Result<TlsAcceptor> {
    let certs = load_certs(&cfg.cert)?;
    let key = load_key(&cfg.key)?;

    let server_cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(Error::Tls)?;

    Ok(TlsAcceptor::from(Arc::new(server_cfg)))
}

fn load_certs(
    path: &Path,
) -> Result<Vec<rustls_pki_types::CertificateDer<'static>>> {
    let pem = std::fs::read(path).map_err(|e| Error::TlsFileRead {
        path: path.to_owned(),
        source: e,
    })?;
    let mut cursor = std::io::Cursor::new(pem);
    let items: Vec<_> = certs(&mut cursor)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::TlsFileRead {
            path: path.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })?;
    if items.is_empty() {
        return Err(Error::TlsNoCert(path.to_owned()));
    }
    Ok(items)
}

fn load_key(path: &Path) -> Result<rustls_pki_types::PrivateKeyDer<'static>> {
    let pem = std::fs::read(path).map_err(|e| Error::TlsFileRead {
        path: path.to_owned(),
        source: e,
    })?;

    // 先尝试 PKCS#8，再尝试传统 RSA 格式
    let mut cursor = std::io::Cursor::new(&pem);
    let mut keys: Vec<_> = pkcs8_private_keys(&mut cursor)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_default();
    if !keys.is_empty() {
        return Ok(rustls_pki_types::PrivateKeyDer::Pkcs8(keys.remove(0)));
    }

    let mut cursor = std::io::Cursor::new(&pem);
    let mut rsa_keys: Vec<_> = rsa_private_keys(&mut cursor)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_default();
    if !rsa_keys.is_empty() {
        return Ok(rustls_pki_types::PrivateKeyDer::Pkcs1(rsa_keys.remove(0)));
    }

    Err(Error::TlsNoKey(path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时 PEM 文件，析构时删掉。
    ///
    /// 没有为此引入 tempfile：这里只需要几个内容可控的文件，名字带上进程 ID
    /// 与序号就足以让并行测试不互相覆盖。
    struct TempPem(std::path::PathBuf);

    impl TempPem {
        fn new(tag: &str, body: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static SEQ: AtomicU32 = AtomicU32::new(0);

            let path = std::env::temp_dir().join(format!(
                "aoproxy-test-{tag}-{}-{}.pem",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, body).expect("写入临时 PEM");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPem {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// 一个肯定不存在的路径。
    fn absent() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aoproxy-test-absent-{}.pem", std::process::id()))
    }

    // ── 证书 ───────────────────────────────────────────────

    #[test]
    fn certs_without_pem_block_report_no_cert() {
        let f = TempPem::new("nocert", "not a certificate\n");
        assert!(matches!(load_certs(f.path()), Err(Error::TlsNoCert(_))));
    }

    #[test]
    fn certs_unreadable_file_reports_read_error() {
        assert!(matches!(
            load_certs(&absent()),
            Err(Error::TlsFileRead { .. })
        ));
    }

    // ── 私钥 ───────────────────────────────────────────────

    /// PKCS#8 与传统 RSA 都解不出来时报 TlsNoKey，而不是把空列表当成功。
    #[test]
    fn key_without_pem_block_reports_no_key() {
        let f = TempPem::new("nokey", "-----BEGIN CERTIFICATE-----\ngarbage\n");
        assert!(matches!(load_key(f.path()), Err(Error::TlsNoKey(_))));
    }

    #[test]
    fn key_unreadable_file_reports_read_error() {
        assert!(matches!(load_key(&absent()), Err(Error::TlsFileRead { .. })));
    }

    // ── build_acceptor ─────────────────────────────────────

    #[test]
    fn acceptor_missing_cert_file_reports_read_error() {
        let key = TempPem::new("key", "whatever\n");
        let cfg = TlsConfig {
            cert: absent(),
            key: key.path().to_owned(),
        };
        assert!(matches!(
            build_acceptor(&cfg),
            Err(Error::TlsFileRead { .. })
        ));
    }

    /// 证书先于私钥校验：私钥同样不可用时，报出来的仍是证书那一条。
    #[test]
    fn acceptor_checks_cert_before_key() {
        let cert = TempPem::new("emptycert", "no pem here\n");
        let cfg = TlsConfig {
            cert: cert.path().to_owned(),
            key: absent(),
        };
        assert!(matches!(build_acceptor(&cfg), Err(Error::TlsNoCert(_))));
    }
}
