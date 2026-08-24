# Day One D1 Encrypted Binary Format

This document specifies the on-disk and over-the-wire binary container used by Day One to store encrypted data, referred to as the D1 binary format ("D1 blob"). The format encapsulates ciphertext together with the metadata required to decrypt and verify it. It supports embedding a protected copy of the symmetric content key, enabling a self-contained artifact for most encrypted objects.

The D1 binary format is used by multiple Day One subsystems, including encrypted journal entries, thumbnails, images, journal private keys, and other secrets. Unless stated otherwise, sizes below are in bytes and all integers are big‑endian.

## Cryptography

- Symmetric encryption: AES‑256 in GCM mode
  - IV: 12 random bytes
  - Authentication tag: 16 bytes
- Asymmetric encryption: RSA‑2048 with PKCS#1 OAEP padding (SHA‑1 for MGF1)

## Binary format versions

The third byte of the header identifies the D1 binary format version. Recognized values:

- 0 – Encrypted content only. No locked key is embedded. Use when the decrypting party already knows the symmetric key by out‑of‑band context.
- 1 – Encrypted content plus an embedded locked symmetric key. The content key is RSA‑encrypted by an asymmetric public key and stored inside the blob.
- 2 – Same as version 1, but the plaintext is first gzip‑compressed, then encrypted. Used for plaintext JSON payloads to reduce size.

## Common use cases

- Format 0 (no embedded key)
  - Journal names (encrypted with the journal vault key)
  - Journal private keys (encrypted with the journal vault key)
  - User private key (encrypted with the user master key)
- Format 1 (embedded locked key) – non‑plaintext user content
  - Encrypted thumbnails
  - Encrypted images
- Format 2 (gzip + embedded locked key) – plaintext user content
  - Encrypted JSON content, e.g., journal entries

## File layout

Each D1 blob is a concatenation of the following fields in order. Fields marked "[v1+ only]" are present only for format versions 1 and 2.

| Size | Field                       | Description                                                                                                                                             |
| ---: | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
|    2 | Magic header                | ASCII `D1` (0x44 0x31)                                                                                                                                  |
|    1 | Crypto schema version       | Currently `0x01`, meaning AES‑256‑GCM                                                                                                                   |
|    1 | Binary format version       | `0x00`, `0x01`, or `0x02` as defined above                                                                                                              |
|   32 | [v1+ only] Fingerprint      | 32 raw bytes identifying the asymmetric key that protects the locked symmetric key                                                                      |
|    2 | [v1+ only] Signature length | Big‑endian Int16; typically `0` or `256`                                                                                                                |
|  var | [v1+ only] Signature        | Signature over the 256‑byte Locked Key field below. When absent (`length=0`), the creator only had the public key (no proof of private key possession). |
|  256 | [v1+ only] Locked Key       | The symmetric content key encrypted with the public key referenced by `Fingerprint` (RSA‑2048‑OAEP).                                                    |
|   12 | IV                          | Random 12‑byte initialization vector for AES‑GCM                                                                                                        |
|  var | Ciphertext                  | AES‑256‑GCM ciphertext of the plaintext (gzip‑compressed first for version 2). Size is the remaining bytes up to the final 32 bytes.                    |
|   16 | GCM tag                     | 16‑byte authentication tag from AES‑GCM                                                                                                                 |
|   16 | Checksum                    | MD5 of all preceding bytes in the blob                                                                                                                  |

Notes:

- For version 2, the plaintext is gzip‑compressed prior to encryption.
- The Ciphertext length is not explicitly stored; it is implied by the file size minus the final 32 bytes for the GCM tag and checksum.

## Fingerprints and signatures

- Fingerprint (asymmetric key): `SHA‑256(DER‑encoded public key bytes)`, stored as the 32 raw hash bytes in the blob.
- Fingerprint (symmetric key): `SHA‑256(key)`, used elsewhere in the system; not stored in D1 blobs unless referenced in other records.
- Locked Key: 256 bytes produced by RSA‑2048‑OAEP encrypting the 32‑byte symmetric content key.
- Signature: When present, `RSA‑SHA256` signature computed over the 256‑byte Locked Key using the corresponding private key. This proves possession of the private key at creation time.

## Decryption procedure

Given a D1 blob:

1. Verify the Magic header equals `D1` and the Crypto schema version is supported (`0x01`).
2. Read the Binary format version.
3. If version is 1 or 2:
   - Read `Fingerprint`, `Signature length`, optional `Signature`, and `Locked Key`.
   - Locate the private key whose fingerprint equals the 32‑byte value.
   - Using that private key, RSA‑OAEP decrypt `Locked Key` to obtain the 32‑byte symmetric content key.
   - If a signature is present, verify the signature over the 256‑byte `Locked Key` using the public key.
4. Read the 12‑byte IV, the trailing 16‑byte GCM tag, and the trailing 16‑byte checksum.
5. Compute MD5 over all bytes preceding the checksum and compare with the stored checksum.
6. Decrypt the ciphertext using AES‑256‑GCM with the derived content key, IV, and the stored GCM tag.
7. If the version is 2, gunzip the resulting plaintext.

For version 0, step 3 is skipped. The decrypting party must supply the correct symmetric content key from context (e.g., the journal vault key or the user master key) and proceed with steps 4–7.

## Validation and integrity

- Integrity and authenticity of the ciphertext are provided by AES‑GCM. If the wrong key or IV is used, GCM authentication fails and decryption MUST be treated as invalid.
- The final MD5 checksum enables quick detection of corruption independent of cryptographic verification.

## Compatibility considerations

- Unrecognized Binary format versions MUST cause the reader to reject the blob.
- Additional crypto schema versions may be introduced in the future. Readers SHOULD treat unknown schema versions as unsupported.
- Writers SHOULD default to version 2 for plaintext JSON payloads and version 1 for binary media. Version 0 is reserved for cases where the symmetric key is already known or managed externally.

## Field summary (reference)

```
Offset  Size  Field
0x0000  2     Magic "D1"
0x0002  1     Crypto schema version (0x01 = AES‑256‑GCM)
0x0003  1     Binary format version (0, 1, or 2)
-- present only when version >= 1 --
0x0004  32    Fingerprint (SHA‑256 of DER public key)
0x0024  2     Signature length (Int16)
0x0026  var   Signature (0 or 256 bytes)
0x0026+ 256   Locked Key (RSA‑OAEP ciphertext of 32‑byte content key)
-- common fields --
...    12     IV (random)
...    var    Ciphertext (AES‑256‑GCM; gzip first for v2)
...    16     GCM tag
...    16     MD5 checksum of all preceding bytes
```

---

This document captures the complete, implementation‑level specification for producing and consuming Day One D1 binary blobs.

## Debug tools

Two browser-based tools are available in this docs folder for working with D1 data:

- [`d1-helper.html`](d1-helper.html) — D1 Blob Inspector: paste base64/hex or drag-drop a `.d1` file to parse and inspect every field, verify the MD5 checksum, and view bytes in hex/base64/UTF-8.
- [`string-encodings.html`](string-encodings.html) — Encoding Converter: real-time three-way conversion between UTF-8, hex, and base64, with byte/bit counts and file drop support.
- [`feature-flag-tool.html`](feature-flag-tool.html) — Feature Flag Tool: interactive bit-toggle UI for building and inspecting Day One entry feature flag bytes.
