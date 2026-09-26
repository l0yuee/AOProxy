//! 入站 TLS 配置：从 PEM 文件构建 rustls ServerConfig。

use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
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

    // PKCS#8、传统 RSA（PKCS#1）与 SEC1（`BEGIN EC PRIVATE KEY`）都认，取文件里的第一把。
    // 只认前两种的话，`openssl ecparam -genkey`、acme.sh 签出的 ECC 私钥都会被当成没有私钥。
    // PEM 本身写坏了（缺结尾行之类）同样报「没有私钥」：对用户来说是同一件事。
    match private_key(&mut std::io::Cursor::new(&pem)) {
        Ok(Some(key)) => Ok(key),
        Ok(None) | Err(_) => Err(Error::TlsNoKey(path.to_owned())),
    }
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

    // ── 私钥格式 ───────────────────────────────────────────

    /// 仅供测试的自签名 P-256 证书与私钥（`openssl ecparam -genkey` + `openssl req -x509`）。
    const EC_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBhTCCASugAwIBAgIUTGE/wDtF6liMGQc1gJ8xNZfTr0swCgYIKoZIzj0EAwIw
FzEVMBMGA1UEAwwMYW9wcm94eS10ZXN0MCAXDTI2MDkyNjEzMzE0M1oYDzIxMjYw
OTAyMTMzMTQzWjAXMRUwEwYDVQQDDAxhb3Byb3h5LXRlc3QwWTATBgcqhkjOPQIB
BggqhkjOPQMBBwNCAARWj312OLxX8snfCSsCLexqu59JK+V2uWrLncJwHZzuGhgj
UnMjjJua5JWJfI0eAhBNqAedvOwIKq6XejFCieEBo1MwUTAdBgNVHQ4EFgQUuaBw
V/e9KqCOE587l24sfTU7OlgwHwYDVR0jBBgwFoAUuaBwV/e9KqCOE587l24sfTU7
OlgwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiByc/e4T5PACEF0
qDUgfaOgoqxrJdgYVkWLhQgRNTFu7gIhAM1e1OUGCsca0ZfQjaQnUDWZbIPcLtfs
VRxVI7dToSPB
-----END CERTIFICATE-----
";

    /// 上面那张证书的私钥，SEC1 格式（`BEGIN EC PRIVATE KEY`）。
    const EC_KEY_SEC1: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIJTDPhngr4Im6VdRRNeFBSo/TJYehJzJ4BJg5Rmoyel4oAoGCCqGSM49
AwEHoUQDQgAEVo99dji8V/LJ3wkrAi3sarufSSvldrlqy53CcB2c7hoYI1JzI4yb
muSViXyNHgIQTagHnbzsCCqul3oxQonhAQ==
-----END EC PRIVATE KEY-----
";

    /// `openssl ecparam -genkey`、acme.sh 的 ECC 证书给出的都是 SEC1 私钥，
    /// 只认 PKCS#8 与 PKCS#1 的话，这类证书一律报「没有私钥」。
    #[test]
    fn acceptor_accepts_sec1_ec_key() {
        let cert = TempPem::new("eccert", EC_CERT);
        let key = TempPem::new("eckey", EC_KEY_SEC1);
        let cfg = TlsConfig {
            cert: cert.path().to_owned(),
            key: key.path().to_owned(),
        };
        if let Err(e) = build_acceptor(&cfg) {
            panic!("SEC1 私钥应当可用：{e}");
        }
    }
}
