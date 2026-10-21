# Test fixtures — NOT for production use

`test_key.pem` / `test_cert.pem` are a throwaway RSA-2048 key pair with a
self-signed certificate (`CN=dian-service-test-signer`), generated only to
exercise the XAdES signing code in unit tests. They carry no trust and must
never be used to sign real documents.
