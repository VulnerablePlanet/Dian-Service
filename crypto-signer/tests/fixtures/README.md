# Test fixtures — NOT for production use

Throwaway RSA-2048 keys and certificates generated with OpenSSL only to exercise the
signing and verification code in tests. They carry no trust and must never be used to
sign real documents.

| File | Purpose |
|---|---|
| `test_ca.pem`, `test_ca_key.pem` | Test root CA (`CN=Dian-Service Test Root CA`) |
| `test_cert.pem`, `test_key.pem` | Signer certificate issued by the test root CA |
| `untrusted_ca.pem`, `untrusted_ca_key.pem` | A second, untrusted root CA |
| `untrusted_cert.pem`, `untrusted_key.pem` | Signer issued by the untrusted CA (negative tests) |
