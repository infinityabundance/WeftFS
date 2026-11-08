module github.com/infinityabundance/WeftFS/adapters/buck2

go 1.23

require (
	github.com/infinityabundance/WeftCodec-Go v0.0.0
	github.com/infinityabundance/WeftFS-Go v0.0.0
)

replace (
	github.com/infinityabundance/WeftCodec-Go => ../../WeftCodec-Go
	github.com/infinityabundance/WeftFS-Go => ../../WeftFS-Go
)

require (
	github.com/klauspost/compress v1.17.11 // indirect
	github.com/klauspost/cpuid/v2 v2.0.9 // indirect
	golang.org/x/sys v0.28.0 // indirect
	lukechampine.com/blake3 v1.4.1 // indirect
)
