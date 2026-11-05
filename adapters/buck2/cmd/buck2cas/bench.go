package main

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"os"
	"path"
	"sort"
	"time"

	"github.com/infinityabundance/WeftFS-Go/store"
	"github.com/infinityabundance/WeftFS/adapters/buck2/cas"
)

type latency struct {
	Count    int     `json:"count"`
	MinMS    float64 `json:"min_ms"`
	MedianMS float64 `json:"median_ms"`
	P95MS    float64 `json:"p95_ms"`
	MaxMS    float64 `json:"max_ms"`
	MeanMS   float64 `json:"mean_ms"`
}

type benchReceipt struct {
	Kind            string             `json:"kind"`
	Schema          string             `json:"schema"`
	TimestampUTC    string             `json:"timestamp_utc"`
	Revision        string             `json:"revision"`
	Revisions       map[string]string  `json:"revisions"`
	Environment     map[string]string  `json:"environment"`
	Label           string             `json:"label"`
	StoreRoot       string             `json:"store_root"`
	SmallActions    int                `json:"small_actions"`
	LargeActions    int                `json:"large_actions"`
	SmallOutputSize int                `json:"small_output_bytes"`
	LargeOutputSize int                `json:"large_output_bytes"`
	Small           map[string]latency `json:"small"`
	Large           map[string]latency `json:"large"`
	Physical        cas.PhysicalBytes  `json:"physical_bytes"`
	Mount           mountPlan          `json:"mount"`
	Methodology     string             `json:"methodology"`
}

func cmdBench(args []string) error {
	fs := flagSet("bench")
	root := fs.String("root", "", "WeftFS store root to create")
	out := fs.String("out", "", "benchmark receipt JSON path")
	small := fs.Int("small", 1000, "number of small-output actions")
	large := fs.Int("large", 100, "number of 1 MiB-output actions")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *out == "" {
		return fmt.Errorf("bench requires --root/--out")
	}
	if *small < 1 || *large < 1 {
		return fmt.Errorf("bench needs --small >= 1 and --large >= 1")
	}
	if err := os.RemoveAll(*root); err != nil {
		return err
	}
	opts := store.DefaultOptions()
	opts.GCGraceSecs = 0
	opts.CheckpointInterval = 256
	fixed := int64(1767225600000000000)
	opts.FixedTimestampNS = &fixed
	s, err := cas.Init(*root, opts)
	if err != nil {
		return err
	}
	defer s.Close()

	const smallSize = 128
	const largeSize = 1 << 20
	smallOut := make([]byte, smallSize)
	largeOut := make([]byte, largeSize)

	action := func(kind string, i int) cas.Action {
		input := []byte(fmt.Sprintf("%s-input-%06d", kind, i))
		return cas.Action{
			ActionType: "bench_" + kind,
			Toolchain:  "weft-bench-1",
			Inputs: []cas.Input{{
				Path:      "src/input.txt",
				ContentID: cas.ContentID(input),
				Bytes:     int64(len(input)),
			}},
			Params: map[string]string{"index": fmt.Sprintf("%d", i)},
		}
	}
	output := func(dst []byte, kind string, i int) []byte {
		fillPattern(dst, uint64(i+1)*0x9e3779b97f4a7c15)
		copy(dst, []byte(fmt.Sprintf("%s-%06d\n", kind, i)))
		return dst
	}

	// Warm-up (not measured): one full publish/fetch cycle.
	warmAction := action("warm", 0)
	warmOut := output(make([]byte, smallSize), "warm", 0)
	if _, _, err := s.StoreAction(warmAction, warmOut, "out/warm.bin"); err != nil {
		return err
	}
	if _, _, err := s.Fetch(mustKey(warmAction), ""); err != nil {
		return err
	}

	smallKeys := make([]string, *small)
	smallOutputs := make([][]byte, *small)
	largeKeys := make([]string, *large)
	largeOutputs := make([][]byte, *large)

	storeNew := make([]time.Duration, 0, *small+*large)
	storeHit := make([]time.Duration, 0, *small+*large)
	fetchVerify := make([]time.Duration, 0, *small+*large)

	publish := func(a cas.Action, data []byte, outPath string) (string, error) {
		start := time.Now()
		record, _, err := s.StoreAction(a, data, outPath)
		storeNew = append(storeNew, time.Since(start))
		if err != nil {
			return "", err
		}
		start = time.Now()
		if _, _, err := s.StoreAction(a, data, outPath); err != nil {
			return "", err
		}
		storeHit = append(storeHit, time.Since(start))
		return record.ActionKey, nil
	}

	for i := 0; i < *small; i++ {
		a := action("small", i)
		data := output(smallOut, "small", i)
		key, err := publish(a, data, fmt.Sprintf("out/small-%06d.bin", i))
		if err != nil {
			return err
		}
		smallKeys[i] = key
		smallOutputs[i] = append([]byte(nil), data...)
	}
	for i := 0; i < *large; i++ {
		a := action("large", i)
		data := output(largeOut, "large", i)
		key, err := publish(a, data, fmt.Sprintf("out/large-%06d.bin", i))
		if err != nil {
			return err
		}
		largeKeys[i] = key
		largeOutputs[i] = append([]byte(nil), data...)
	}

	for i, key := range smallKeys {
		start := time.Now()
		if _, data, err := s.Fetch(key, ""); err != nil {
			return err
		} else if _, err := s.Verify(key, data); err != nil {
			return err
		}
		_ = i
		fetchVerify = append(fetchVerify, time.Since(start))
	}
	largeFetch := make([]time.Duration, 0, *large)
	for _, key := range largeKeys {
		start := time.Now()
		if _, data, err := s.Fetch(key, ""); err != nil {
			return err
		} else if _, err := s.Verify(key, data); err != nil {
			return err
		}
		largeFetch = append(largeFetch, time.Since(start))
	}

	// Miss latency: fetch keys that were never stored.
	miss := make([]time.Duration, 0, 64)
	for i := 0; i < 64; i++ {
		a := action("miss", i)
		key, err := cas.DeriveKey(a)
		if err != nil {
			return err
		}
		start := time.Now()
		if _, _, err := s.Fetch(key.ActionKey, ""); err == nil {
			return fmt.Errorf("miss key %d unexpectedly hit", i)
		}
		miss = append(miss, time.Since(start))
	}
	smallMiss := make([]time.Duration, 0, 64)
	for i := 0; i < 64; i++ {
		a := action("small-miss", i)
		key, err := cas.DeriveKey(a)
		if err != nil {
			return err
		}
		start := time.Now()
		if _, _, err := s.Fetch(key.ActionKey, ""); err == nil {
			return fmt.Errorf("small miss key %d unexpectedly hit", i)
		}
		smallMiss = append(smallMiss, time.Since(start))
	}

	// A context over one 1 MiB artifact gives the shell a real mount target.
	if _, err := s.Snapshot("bench"); err != nil {
		return err
	}
	largeRecord, _, err := s.Fetch(largeKeys[0], "")
	if err != nil {
		return err
	}
	ctx, err := s.CreateContext(largeKeys[0], "by:action="+largeKeys[0], "bench", false)
	if err != nil {
		return err
	}
	physical, err := s.PhysicalBytes()
	if err != nil {
		return err
	}
	head := s.Head()
	generation := s.Generation()

	base := path.Base(largeRecord.OutputPath)
	mount := mountPlan{
		StoreRoot:       *root,
		ContextName:     ctx.Name,
		ContextRootID:   ctx.ContextRootID,
		SnapshotID:      ctx.SnapshotID,
		Selector:        ctx.Selector,
		OutputObjectID:  largeRecord.OutputObjectID,
		OutputStorePath: path.Join(cas.OutputsDir, largeKeys[0], largeRecord.OutputPath),
		OutputBasename:  base,
		ExpectedName:    base + "--" + largeRecord.OutputObjectID[:8],
		ContentID:       largeRecord.OutputContentID,
		SHA256:          sha256Hex(largeOutputs[0]),
		Bytes:           largeRecord.OutputBytes,
	}

	receipt := benchReceipt{
		Kind:            "buck2-cas-store-bench",
		Schema:          "weft.buck2-cas-store.bench/1",
		TimestampUTC:    time.Now().UTC().Format(time.RFC3339),
		Revision:        os.Getenv("WEFT_REPO_WEFTFS"),
		Revisions:       envRevisions(),
		Environment:     envCourt(),
		Label:           "single host, synthetic deterministic fixtures, local WeftFS store, Docker court; latency is the adapter library path with one open writer, not a CLI or fleet claim",
		StoreRoot:       *root,
		SmallActions:    *small,
		LargeActions:    *large,
		SmallOutputSize: smallSize,
		LargeOutputSize: largeSize,
		Small: map[string]latency{
			"store_new":    summarize(storeNew[:*small]),
			"store_hit":    summarize(storeHit[:*small]),
			"fetch_verify": summarize(fetchVerify),
			"store_miss":   summarize(smallMiss),
		},
		Large: map[string]latency{
			"store_new":    summarize(storeNew[*small:]),
			"store_hit":    summarize(storeHit[*small:]),
			"fetch_verify": summarize(largeFetch),
			"store_miss":   summarize(miss),
		},
		Physical:    physical,
		Mount:       mount,
		Methodology: "per-operation wall clock with one open store writer; store_new = output blob + action record + two relations published; store_hit = idempotent repeat; fetch_verify = Fetch plus an independent re-hash in Verify; store_miss = Fetch of a never-stored key; physical bytes = sum of file sizes under the store root after all writes. Mount-read latency is measured separately by the shell court.",
	}
	if err := writeJSONFile(*out, receipt); err != nil {
		return err
	}
	env := fmt.Sprintf(
		"ACTION_KEY=%s\nSTORE=%s\nCONTEXT_NAME=%s\nCONTEXT_ROOT_ID=%s\nSNAPSHOT_ID=%s\nOUTPUT_OBJECT_ID=%s\nOUTPUT_STORE_PATH=%s\nEXPECTED_NAME=%s\nCONTENT_ID=%s\nOUTPUT_SHA256=%s\nOUTPUT_BYTES=%d\n",
		largeKeys[0], mount.StoreRoot, mount.ContextName, mount.ContextRootID, mount.SnapshotID,
		mount.OutputObjectID, mount.OutputStorePath, mount.ExpectedName,
		mount.ContentID, mount.SHA256, mount.Bytes)
	if err := os.WriteFile(*out+".mount.env", []byte(env), 0o644); err != nil {
		return err
	}
	fmt.Printf("buck2-cas bench: %d small + %d large actions; head=%s generation=%d; receipt %s\n",
		*small, *large, head, generation, *out)
	return nil
}

func mustKey(action cas.Action) string {
	key, err := cas.DeriveKey(action)
	if err != nil {
		panic(err)
	}
	return key.ActionKey
}

func sha256Hex(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func fillPattern(dst []byte, seed uint64) {
	x := seed
	for i := 0; i < len(dst); i += 8 {
		x ^= x << 13
		x ^= x >> 7
		x ^= x << 17
		var word [8]byte
		binary.LittleEndian.PutUint64(word[:], x)
		copy(dst[i:], word[:])
	}
}

func summarize(samples []time.Duration) latency {
	if len(samples) == 0 {
		return latency{}
	}
	values := make([]float64, len(samples))
	var sum float64
	for i, sample := range samples {
		values[i] = float64(sample.Nanoseconds()) / 1e6
		sum += values[i]
	}
	sort.Float64s(values)
	pick := func(p float64) float64 {
		index := int(p*float64(len(values)) + 0.999999)
		if index < 1 {
			index = 1
		}
		if index > len(values) {
			index = len(values)
		}
		return values[index-1]
	}
	return latency{
		Count:    len(values),
		MinMS:    values[0],
		MedianMS: pick(0.50),
		P95MS:    pick(0.95),
		MaxMS:    values[len(values)-1],
		MeanMS:   sum / float64(len(values)),
	}
}

func envRevisions() map[string]string {
	return map[string]string{
		"WeftFS":       os.Getenv("WEFT_REPO_WEFTFS"),
		"WeftCodec":    os.Getenv("WEFT_REPO_WEFTCODEC"),
		"WeftCodec-Go": os.Getenv("WEFT_REPO_WEFTCODEC_GO"),
		"WeftFS-Go":    os.Getenv("WEFT_REPO_WEFTFS_GO"),
	}
}

func envCourt() map[string]string {
	return map[string]string{
		"court_image":    os.Getenv("WEFT_COURT_IMAGE"),
		"court_image_id": os.Getenv("WEFT_COURT_IMAGE_ID"),
		"kernel":         os.Getenv("WEFT_COURT_KERNEL"),
		"memory":         os.Getenv("WEFT_COURT_MEMORY"),
		"cpus":           os.Getenv("WEFT_COURT_CPUS"),
		"pids":           os.Getenv("WEFT_COURT_PIDS"),
		"nproc":          os.Getenv("WEFT_COURT_NPROC"),
		"docker_server":  os.Getenv("WEFT_DOCKER_SERVER"),
	}
}
