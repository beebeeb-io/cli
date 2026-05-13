# Encryption Performance

Your files are encrypted before they leave your device. Here's exactly what that costs.

## The short answer

Encryption is not the bottleneck. Your internet connection is. On modern hardware, encryption runs at 140+ MB/s. Most home connections max out at 10-50 MB/s upload. The crypto is never the thing you're waiting for.

## What happens to your files

1. Files are split into 1 MB chunks.
2. Each chunk is encrypted with AES-256-GCM -- the same standard used by banks and governments.
3. Each chunk gets a unique 12-byte nonce (a random number) so identical files never produce identical ciphertext.
4. A 16-byte authentication tag is appended to detect tampering.
5. The encrypted chunk is wrapped in a JSON envelope with base64 encoding.

## The real overhead

Per chunk, encryption adds 28 bytes of crypto overhead. That's negligible.

The real cost is base64 JSON encoding: a 33% size increase. A 1 GB file becomes roughly 1.33 GB on the wire.

We're working on a binary format to eliminate this. For now, the 33% overhead is the price of using a format that's easy to debug and verify.

## Real benchmarks from `bb speedtest`

| Operation | Speed |
|---|---|
| Encrypt | 140 MB/s |
| Decrypt | 143 MB/s |
| Key derivation | 1 ms |

Your network is 3x slower than encryption. The crypto is never the thing you're waiting for.

## What a real upload looks like

| File size | Crypto time | Network time (50 Mbps) | Total | Overhead vs unencrypted |
|---|---|---|---|---|
| 10 MB | 0.07s | 2.1s | 2.2s | +5% |
| 100 MB | 0.7s | 21s | 22s | +5% |
| 1 GB | 7s | 213s | 220s | +3% |

The percentage overhead decreases as files get larger because network latency dominates.

## What we don't do

- We don't re-encrypt on download. Your key, your device, one operation.
- We don't add watermarks or metadata.
- The server never sees plaintext -- not even file names.

You can verify this yourself: `bb speedtest` measures everything on your machine.

## Sync performance

Uploads run with 4 parallel workers by default. On a fast connection, bump it:

```
bb sync --concurrency 8
```

Server-side throttle limits apply per plan:

| Plan | Upload rate limit |
|---|---|
| Free | 10 MB/s |
| Pro | 50 MB/s |
| Business | Unlimited |

## Run it yourself

```
bb speedtest
```

It runs encryption, decryption, and key derivation benchmarks locally and prints your actual numbers. No network required.
