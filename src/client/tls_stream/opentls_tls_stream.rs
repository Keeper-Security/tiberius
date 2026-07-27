use crate::{
    client::{config::Config, TrustConfig},
    error::{Error, IoErrorKind},
};
use futures_util::io::{AsyncRead, AsyncWrite};
pub(crate) use opentls::async_io::{TlsConnector, TlsStream};
use opentls::Certificate;
use std::fs;
use tracing::{event, Level};

pub(crate) async fn create_tls_stream<S: AsyncRead + AsyncWrite + Unpin + Send>(
    config: &Config,
    stream: S,
) -> crate::Result<TlsStream<S>> {
    let mut builder = TlsConnector::new();

    match &config.trust {
        TrustConfig::CaCertificateLocation(path) => {
            if let Ok(buf) = fs::read(path) {
                let cert = match path.extension() {
                        Some(ext)
                        if ext.to_ascii_lowercase() == "pem"
                            || ext.to_ascii_lowercase() == "crt" =>
                            {
                                Some(Certificate::from_pem(&buf)?)
                            }
                        Some(ext) if ext.to_ascii_lowercase() == "der" => {
                            Some(Certificate::from_der(&buf)?)
                        }
                        Some(_) | None => return Err(Error::Io {
                            kind: IoErrorKind::InvalidInput,
                            message: "Provided CA certificate with unsupported file-extension! Supported types are pem, crt and der.".to_string()}),
                    };
                if let Some(c) = cert {
                    builder = builder.add_root_certificate(c);
                }
            } else {
                return Err(Error::Io {
                    kind: IoErrorKind::InvalidData,
                    message: "Could not read provided CA certificate!".to_string(),
                });
            }
        }
        TrustConfig::TrustAll => {
            event!(
                Level::WARN,
                "Trusting the server certificate without validation."
            );

            builder = builder.danger_accept_invalid_certs(true);
            builder = builder.danger_accept_invalid_hostnames(true);
            builder = builder.use_sni(false);
        }
        TrustConfig::Default => {
            event!(Level::INFO, "Using default trust configuration.");

            // The vendored OpenSSL discovers root certificates by probing
            // Unix filesystem paths (openssl-probe), which finds nothing on
            // Windows — its trust anchors live in registry-backed
            // certificate stores. Load the ROOT store into the connector so
            // certificate validation can succeed; the current-user view is
            // a composite that includes the local-machine store, so certs
            // installed via certlm.msc (machine) or GP-pushed (e.g. Zscaler)
            // are picked up automatically. Individual certificates OpenSSL
            // cannot parse are skipped, matching what rustls-native-certs does.
            //
            // NOTE: open_current_user("ROOT") is correct for user-session
            // processes (e.g. the KeeperDB desktop app). A Windows service
            // would need open_local_machine("ROOT") instead, since services
            // run in session 0 and the current-user store may be empty there.
            #[cfg(windows)]
            match schannel::cert_store::CertStore::open_current_user("ROOT") {
                Ok(store) => {
                    for windows_cert in store.certs() {
                        match Certificate::from_der(windows_cert.to_der()) {
                            Ok(root_cert) => {
                                builder = builder.add_root_certificate(root_cert);
                            }
                            Err(e) => {
                                event!(
                                    Level::WARN,
                                    "Skipping an unparseable certificate from the Windows ROOT store: {}",
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    event!(
                        Level::WARN,
                        "Could not open the Windows ROOT certificate store; certificate validation will have no trusted roots: {}",
                        e
                    );
                }
            }
        }
    }

    Ok(builder.connect(config.get_host(), stream).await?)
}
