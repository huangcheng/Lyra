//! Mark Verifying Authority (MVA) trust anchors for BIMI VMC validation.
//!
//! Each constant carries its download source and the officially published
//! SHA-256 fingerprint it was verified against (re-check with:
//! `openssl x509 -in root.pem -fingerprint -sha256 -noout`).

/// DigiCert Verified Mark Root CA (valid until 23/Sep/2049).
///
/// Source: https://cacerts.digicert.com/DigiCertVerifiedMarkRootCA.crt.pem
/// (linked from DigiCert's official root-certificate index,
/// https://knowledge.digicert.com/general-information/digicert-trusted-root-authority-certificates)
///
/// SHA-256 fingerprint (matches the value published on that page):
/// 50:43:86:C9:EE:89:32:FE:CC:95:FA:DE:42:7F:69:C3:E2:53:4B:73:10:48:9E:30:0F:EE:44:8E:33:C4:6B:42
pub(crate) const DIGICERT_VMC_ROOT_PEM: &str = concat!(
    "-----BEGIN CERTIFICATE-----\n",
    "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\n",
    "0wberd/tg3N0dJsSSznZjwYB\n",
    "-----END CERTIFICATE-----\n",
);

/// Entrust Verified Mark Root Certification Authority - VMCR1
/// (RSA 4096, SHA-512; valid until 30/Dec/2040).
///
/// Source: MVA root collection mirrored at
/// https://gist.github.com/seanthegeek/fd2d596d575b0815eee2c461f91a43a2
/// (BIMI-community "MVACAs.pem").
///
/// SHA-256 fingerprint verified against the officially published value on
/// Entrust's root-certificate index
/// (https://www.entrust.com/knowledgebase/ssl/entrust-root-certificates):
/// 78:31:D9:5A:47:D4:25:08:CD:5C:9E:62:64:F9:09:6B:AC:19:F0:4E:B9:B7:C8:BD:D3:5F:FF:C7:1C:18:96:17
pub(crate) const ENTRUST_VMC_ROOT_PEM: &str = concat!(
    "-----BEGIN CERTIFICATE-----\n",
    "MIIFpDCCA4ygAwIBAgIUdDkAvVsH/GPX6RUEUsibtwFoBGMwDQYJKoZIhvcNAQEN\n",
    "BQAwajELMAkGA1UEBhMCVVMxFjAUBgNVBAoMDUVudHJ1c3QsIEluYy4xQzBBBgNV\n",
    "BAMMOkVudHJ1c3QgVmVyaWZpZWQgTWFyayBSb290IENlcnRpZmljYXRpb24gQXV0\n",
    "aG9yaXR5IC0gVk1DUjEwHhcNMjEwNTA3MTMzMTQ4WhcNNDAxMjMwMTMzMTQ4WjBq\n",
    "MQswCQYDVQQGEwJVUzEWMBQGA1UECgwNRW50cnVzdCwgSW5jLjFDMEEGA1UEAww6\n",
    "RW50cnVzdCBWZXJpZmllZCBNYXJrIFJvb3QgQ2VydGlmaWNhdGlvbiBBdXRob3Jp\n",
    "dHkgLSBWTUNSMTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBAL1S/GJt\n",
    "w3EI3J6CcvhFRTpAZUWnTTgj/0n04xEBEZu4bVR8JlYFnadfTm+CLyUkLCU7Ipoq\n",
    "Y0D6jDcD2skWuXnSsTeQtiFIDiGJH4c6QWmNXw1mZDlNhLse0q2sfXCxlAlfBr9M\n",
    "c2KOrNUFk36Ld6VQEZOb/R1aU/GwbfN0A/8mDQSRoIHlFgWrtqYwORBF8MFqv4a2\n",
    "MvE858h6KaKaBy/8TVMuvuYZ32sa1yGHibAP8Kr0YaFHiK+iLJxnJccjyXjzfLMY\n",
    "zQ9rt/UuAlHTIXsNJE+ZYo4O3unPMK25lHGenEVRWOZiIVm/Kl/JdxqxETZRDwCS\n",
    "KiXlHcXHkFTtvOQRQ5qcR0p1MrgIUzrzVZSqIM9O92q/tOgsKyv+GoTchBVrn57N\n",
    "q2EsarFP2zQqLlSC2Z1KTO+c2bjf90BbDL+mlxYycbRfHc5GZ9LXnxmilBSLz5m0\n",
    "MGq1uamqC5pkri7V2tDe+Mb3FQcD/yDhaTs7jL/ODv1dYv3OCQ9YzUZtgvWSqzi9\n",
    "rHt2yFotR+XM4BB8n3De1QKnFLZB7s469xRUUjFIJ07lOapBTuZWsarECtAWloW0\n",
    "uKr0+LLO2nNX3t92qbPcmEu9dI2Z7K94VLZ/ONhVuroPLlzJ36tTP4zLXo5GXaMY\n",
    "UnosKa2jkMu4QfII9g8NkHYRmUBk26vnCS2JAgMBAAGjQjBAMB0GA1UdDgQWBBRz\n",
    "I1Z7K3hFgJq4wnzMpYY5iyZ4xTAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQE\n",
    "AwIBhjANBgkqhkiG9w0BAQ0FAAOCAgEASMtZ53rMG+bDGJbYCCucv9KzqgCL166R\n",
    "V9Eq0zQOXITcfVpJmFeBLq/rMzXCOdWNTdPx6yNdeWk6OW5GullwzJhsutiHcryQ\n",
    "acDRHEnMf0LodOy2TWLWWonsyctVHa2PtbcViWZ7opctUTmsK6JdMHCAOZHH64Nj\n",
    "0Vr0VAaLf/A/fF+ZlU1IbcU1Gi/FBJudrT7YD2ISmIukCv7hsdhAtg9TuOVkWKl0\n",
    "gfDadejTU4l5VlT58ofkxg5aAL2XPJf7ywKzgWlWgLpIZWMsn6+7dOiAq7GqsVKN\n",
    "zEKTyAc3Hs42hpKtHvZFDGJ3mGVUNPNjEiH7OZ7q1gKB7eysWDcUp+IMLn+nukDo\n",
    "JUb9H+TiF0i7Zo+roPyzv+fy2tJuioF9NkVGqqOrfTLxOo10gCM42ba4Mf6PkvC4\n",
    "FkkNb7q7OVi6nsO8pUNJ+PagwFyMLp0vqd4aDTUruey7tKz76SM1D6rN9WJFgZsp\n",
    "Yj4dKCQPde32Jd9/Sk6G5lHmIbAqqNYLqPRBxByVSBg9+11jMi7e1kIkMQV3wndB\n",
    "ntRKjHU8Hd0J/UcK8veLaRR2XECTx0I9I31Fis4Q0cSVz+4oXWGBuaREKEut10q2\n",
    "cAWvd1qUOjLlA4LsxKpVMvc+loyIy5s0+IfcqN4GHYjBKK+m+GWs/u2Q4BiKeVxw\n",
    "gkWRxBsyYPA=\n",
    "-----END CERTIFICATE-----\n",
);
