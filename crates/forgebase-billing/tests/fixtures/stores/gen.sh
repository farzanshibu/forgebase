#!/bin/sh
# Regenerates the FB-72 test-only keys and certificates (never used outside tests).
# A fake "Apple" chain (P-384 root and intermediate, P-256 leaf with Apple's
# marker extensions), a rogue leaf without the leaf marker, a second root that
# Apple never issued, Google service-account and push-signer RSA keys, and
# WeChat Pay merchant and platform RSA keys with a platform certificate.
set -eu
cd "$(dirname "$0")"
D="-days 36500"
ext() { printf '%s\n' "$@" > ext.cnf; }

# Apple-like chain
openssl ecparam -name secp384r1 -genkey -noout -out apple-root.key
openssl req -new -x509 -key apple-root.key -sha384 $D -subj "/CN=Test Apple Root CA - G3/O=ForgeBase Test" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -set_serial 1 -not_before 20250101000000Z -out apple-root.pem
openssl ecparam -name secp384r1 -genkey -noout -out apple-int.key
openssl req -new -key apple-int.key -subj "/CN=Test Apple Worldwide Developer Relations CA - G6/O=ForgeBase Test" -out apple-int.csr
ext "basicConstraints=critical,CA:TRUE,pathlen:0" "keyUsage=critical,keyCertSign,cRLSign" "1.2.840.113635.100.6.2.1=ASN1:NULL"
openssl x509 -req -in apple-int.csr -CA apple-root.pem -CAkey apple-root.key -sha384 $D -set_serial 2 \
  -not_before 20250101000000Z -extfile ext.cnf -out apple-int.pem
openssl ecparam -name prime256v1 -genkey -noout -out apple-leaf.ec
openssl pkcs8 -topk8 -nocrypt -in apple-leaf.ec -out apple-leaf.key
openssl req -new -key apple-leaf.key -subj "/CN=Test Prod ECC Mac App Store and iTunes Store Receipt Signing/O=ForgeBase Test" -out apple-leaf.csr
ext "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" "1.2.840.113635.100.6.11.1=ASN1:NULL"
openssl x509 -req -in apple-leaf.csr -CA apple-int.pem -CAkey apple-int.key -sha256 $D -set_serial 3 \
  -not_before 20250101000000Z -extfile ext.cnf -out apple-leaf.pem
# A leaf of the same chain without the receipt-signing marker (a developer certificate).
ext "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature"
openssl x509 -req -in apple-leaf.csr -CA apple-int.pem -CAkey apple-int.key -sha256 $D -set_serial 4 \
  -not_before 20250101000000Z -extfile ext.cnf -out apple-rogue-leaf.pem
# A chain Apple never issued: same shape, other root.
openssl ecparam -name secp384r1 -genkey -noout -out forged-root.key
openssl req -new -x509 -key forged-root.key -sha384 $D -subj "/CN=Test Apple Root CA - G3/O=ForgeBase Test" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -set_serial 1 -not_before 20250101000000Z -out forged-root.pem
ext "basicConstraints=critical,CA:TRUE,pathlen:0" "keyUsage=critical,keyCertSign,cRLSign" "1.2.840.113635.100.6.2.1=ASN1:NULL"
openssl x509 -req -in apple-int.csr -CA forged-root.pem -CAkey forged-root.key -sha384 $D -set_serial 5 \
  -not_before 20250101000000Z -extfile ext.cnf -out forged-int.pem
ext "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" "1.2.840.113635.100.6.11.1=ASN1:NULL"
openssl x509 -req -in apple-leaf.csr -CA forged-int.pem -CAkey apple-int.key -sha256 $D -set_serial 6 \
  -not_before 20250101000000Z -extfile ext.cnf -out forged-leaf.pem
# App Store Connect API key (.p8)
openssl ecparam -name prime256v1 -genkey -noout -out asc.ec
openssl pkcs8 -topk8 -nocrypt -in asc.ec -out asc-key.p8

# Google: service account key and the push (OIDC) signer key
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out google-sa.key
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out google-signer.key
openssl rsa -in google-signer.key -pubout -RSAPublicKey_out -outform DER -out google-signer.rsapub.der

# WeChat Pay: merchant key, platform key and certificate, platform public key
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out wechat-merchant.key
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out wechat-platform.key
openssl req -new -x509 -key wechat-platform.key -sha256 $D -subj "/CN=Tenpay.com Root CA Test/O=ForgeBase Test" \
  -set_serial 0x5157F09EFDC096DE15EBE81A47057A7232F1B8E1 -not_before 20250101000000Z -out wechat-platform.pem
openssl pkey -in wechat-platform.key -pubout -out wechat-platform-pub.pem
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out wechat-other.key
openssl pkey -in wechat-other.key -pubout -out wechat-other-pub.pem
rm -f *.csr *.ec ext.cnf *.srl
