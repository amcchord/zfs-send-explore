# Native-encrypted pool fixture

`native-encrypted-pool.img.zst` is a losslessly recompressed copy of `d0.img`
from `testdata/crypto/encrypted.tar.zst` in
[`go-filesystems/zfs`](https://github.com/go-filesystems/zfs), commit
`c39926605b9c5b159b1526ae1193c4d534a2cb03`. The source fixture was created
with OpenZFS 2.2.2 and is distributed under the BSD 3-Clause license reproduced
in `native-encrypted-pool.LICENSE`.

The 128 MiB single-file vdev contains `encpool/secret`, an `aes-256-gcm`
dataset using `keyformat=passphrase`, `pbkdf2iters=100000`, and passphrase
`hunter2!`. Its two known payloads are:

- `/greeting.txt`: 20 bytes, SHA-256
  `fb13243e8d0033038d1740d8bb6cc0c9f34dd7087f9de133c6813530bb36d042`
- `/blob.bin`: 8192 bytes, SHA-256
  `99b3de991bdff384c8489a793fb8698ba4002fbff01acb2c20dc0b79dc8cdf42`

The checked-in Zstandard frame has SHA-256
`a385c16289cdaffe09b2ee8f77fa3d7b29ed1cf17b063e112b2f33c283f4decd`.
The expanded image has SHA-256
`41c6c49389123d93824b8d2c94c8a3959116e8b007becb079daa9bcbf2e476d6`.
