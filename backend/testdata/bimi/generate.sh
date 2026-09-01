#!/usr/bin/env bash
# Regenerate the BIMI/VMC test fixtures committed in this directory.
#
# Builds a throwaway "Test MVA Root CA" (plus an unrelated "Other Root CA"),
# signs leaf certificates with openssl ca (tiny in-script CA db), revokes one
# leaf, and emits a CRL. Only public PEM artifacts are committed; all private
# keys live in a temp dir that is deleted on exit.
#
# Fixtures produced:
#   test-root.pem                 self-signed Test MVA Root CA (trust anchor)
#   test-leaf-example-com.pem     SAN example.com + logotype OID 1.3.6.1.5.5.7.1.12
#   test-leaf-other-com.pem       SAN other.com + logotype OID
#   test-leaf-no-logotype.pem     SAN example.com, no logotype OID
#   test-leaf-expired.pem         SAN example.com + logotype OID, expired 2021
#   test-leaf-revoked.pem         SAN example.com + logotype OID + cDP, revoked
#   test-root.crl.pem             CRL signed by test root, lists the revoked leaf
#   other-root.pem                unrelated self-signed root (untrusted anchor)
#   other-leaf-example-com.pem    SAN example.com + logotype OID, signed by other root
#
# Logotype extension value is a DER-encoded empty SEQUENCE payload
# (DER:30:03:02:01:00) — presence of the OID is the validation gate, the
# content is never parsed.
set -euo pipefail
cd "$(dirname "$0")"

CA="$(mktemp -d)"
trap 'rm -rf "$CA"' EXIT
mkdir -p "$CA/newcerts"
touch "$CA/index.txt"
echo 1000 > "$CA/serial"
echo 1000 > "$CA/crlnumber"

cat > "$CA/openssl.cnf" <<'EOF'
[ ca ]
default_ca = CA_test

[ CA_test ]
dir               = CADIR_PLACEHOLDER
database          = $dir/index.txt
new_certs_dir     = $dir/newcerts
certificate       = $dir/root.pem
private_key       = $dir/root.key
serial            = $dir/serial
crlnumber         = $dir/crlnumber
default_md        = sha256
default_days      = 3650
default_crl_days  = 3650
policy            = policy_any
unique_subject    = no

[ policy_any ]
commonName = supplied

[ req ]
distinguished_name = dn
prompt             = no

[ dn ]
CN = placeholder

[ v3_logotype ]
subjectAltName      = DNS:example.com
basicConstraints    = CA:false
keyUsage            = digitalSignature
1.3.6.1.5.5.7.1.12  = DER:30:03:02:01:00

[ v3_other ]
subjectAltName      = DNS:other.com
basicConstraints    = CA:false
keyUsage            = digitalSignature
1.3.6.1.5.5.7.1.12  = DER:30:03:02:01:00

[ v3_nologo ]
subjectAltName      = DNS:example.com
basicConstraints    = CA:false
keyUsage            = digitalSignature

[ v3_cdp ]
subjectAltName         = DNS:example.com
basicConstraints       = CA:false
keyUsage               = digitalSignature
crlDistributionPoints  = URI:http://vmc.example.com/test-root.crl
1.3.6.1.5.5.7.1.12     = DER:30:03:02:01:00
EOF
sed -i '' "s|CADIR_PLACEHOLDER|$CA|" "$CA/openssl.cnf"

# ── Test MVA root ────────────────────────────────────────────────────
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$CA/root.key" -out "$CA/root.pem" \
    -days 3650 -subj "/CN=Test MVA Root CA" \
    -addext "basicConstraints=critical,CA:true" \
    -addext "keyUsage=critical,keyCertSign,cRLSign"
cp "$CA/root.pem" test-root.pem

sign_leaf() { # csr_cn ext_section out_pem [extra openssl ca args...]
    local cn="$1" ext="$2" out="$3"; shift 3
    openssl req -new -newkey rsa:2048 -nodes \
        -keyout "$CA/leaf.key" -out "$CA/leaf.csr" -subj "/CN=$cn"
    openssl ca -batch -config "$CA/openssl.cnf" -extensions "$ext" \
        -notext -in "$CA/leaf.csr" -out "$CA/leaf.pem" "$@" >/dev/null 2>&1
    # openssl ca output prefixes the PEM with text unless -notext; strip
    # everything before the first PEM header just in case.
    openssl x509 -in "$CA/leaf.pem" -out "$out"
}

sign_leaf example.com v3_logotype test-leaf-example-com.pem
sign_leaf example.com v3_other    test-leaf-other-com.pem
sign_leaf example.com v3_nologo   test-leaf-no-logotype.pem
sign_leaf example.com v3_logotype test-leaf-expired.pem \
    -startdate 20200101000000Z -enddate 20210101000000Z
sign_leaf example.com v3_cdp      test-leaf-revoked.pem

# ── Revocation + CRL ─────────────────────────────────────────────────
openssl ca -config "$CA/openssl.cnf" -revoke test-leaf-revoked.pem \
    -crl_reason keyCompromise >/dev/null 2>&1
openssl ca -config "$CA/openssl.cnf" -gencrl -out "$CA/test-root.crl.pem" \
    >/dev/null 2>&1
cp "$CA/test-root.crl.pem" test-root.crl.pem

# ── Untrusted other root + leaf ──────────────────────────────────────
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$CA/other-root.key" -out other-root.pem \
    -days 3650 -subj "/CN=Other Root CA" \
    -addext "basicConstraints=critical,CA:true" \
    -addext "keyUsage=critical,keyCertSign,cRLSign"

cat > "$CA/other.cnf" <<EOF
[ v3_logotype ]
subjectAltName      = DNS:example.com
basicConstraints    = CA:false
keyUsage            = digitalSignature
1.3.6.1.5.5.7.1.12  = DER:30:03:02:01:00
EOF
openssl req -new -newkey rsa:2048 -nodes \
    -keyout "$CA/other-leaf.key" -out "$CA/other-leaf.csr" -subj "/CN=example.com"
openssl x509 -req -in "$CA/other-leaf.csr" \
    -CA other-root.pem -CAkey "$CA/other-root.key" -CAcreateserial \
    -CAserial "$CA/other-root.srl" \
    -days 3650 -extfile "$CA/other.cnf" -extensions v3_logotype \
    -out other-leaf-example-com.pem 2>/dev/null

echo "Fixtures regenerated:"
ls -1 ./*.pem
