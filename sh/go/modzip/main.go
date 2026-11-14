// Command modzip writes one version of a Go module in the GOPROXY layout,
// <out>/<module>/@v/<version>.{info,mod,zip}, and prints its go.sum lines.
//
// The go command validates a module zip's layout and hashes its files, so the
// zip comes from golang.org/x/mod/zip, the code the go command itself uses.
package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"golang.org/x/mod/modfile"
	"golang.org/x/mod/module"
	"golang.org/x/mod/semver"
	"golang.org/x/mod/sumdb/dirhash"
	modzip "golang.org/x/mod/zip"
)

func main() {
	dir := flag.String("dir", "", "module root, holding go.mod")
	version := flag.String("version", "", "canonical semantic version, such as v1.2.3")
	at := flag.String("time", "", "RFC 3339 time of the version's commit, for the .info file")
	out := flag.String("out", "", "directory to write <module>/@v/ under")
	flag.Parse()

	if *dir == "" || *version == "" || *at == "" || *out == "" || flag.NArg() != 0 {
		flag.Usage()
		os.Exit(2)
	}
	if err := run(*dir, *version, *at, *out); err != nil {
		fmt.Fprintln(os.Stderr, "modzip:", err)
		os.Exit(1)
	}
}

func run(dir, version, at, out string) error {
	if semver.Canonical(version) != version {
		return fmt.Errorf("%q is not a canonical semantic version", version)
	}
	when, err := time.Parse(time.RFC3339, at)
	if err != nil {
		return err
	}

	gomod, err := os.ReadFile(filepath.Join(dir, "go.mod"))
	if err != nil {
		return err
	}
	path := modfile.ModulePath(gomod)
	if path == "" {
		return fmt.Errorf("%s/go.mod declares no module path", dir)
	}
	mv := module.Version{Path: path, Version: version}
	if err := module.Check(path, version); err != nil {
		return err
	}

	escPath, err := module.EscapePath(path)
	if err != nil {
		return err
	}
	escVersion, err := module.EscapeVersion(version)
	if err != nil {
		return err
	}
	dst := filepath.Join(out, filepath.FromSlash(escPath), "@v")
	if err := os.MkdirAll(dst, 0o755); err != nil {
		return err
	}
	base := filepath.Join(dst, escVersion)

	info, err := json.Marshal(struct {
		Version string
		Time    time.Time
	}{version, when.UTC()})
	if err != nil {
		return err
	}
	if err := create(base+".mod", func(w io.Writer) error { _, err := w.Write(gomod); return err }); err != nil {
		return err
	}
	if err := create(base+".zip", func(w io.Writer) error { return modzip.CreateFromDir(w, mv, dir) }); err != nil {
		return err
	}
	if err := create(base+".info", func(w io.Writer) error { _, err := w.Write(info); return err }); err != nil {
		return err
	}

	sum, err := dirhash.HashZip(base+".zip", dirhash.Hash1)
	if err != nil {
		return err
	}
	modSum, err := dirhash.Hash1([]string{"go.mod"}, func(string) (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader(gomod)), nil
	})
	if err != nil {
		return err
	}
	fmt.Printf("%s %s %s\n%s %s/go.mod %s\n", path, version, sum, path, version, modSum)
	return nil
}

// create writes a new file, refusing to replace one: a published version never changes.
func create(name string, write func(io.Writer) error) error {
	f, err := os.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
	if err != nil {
		return err
	}
	if err := write(f); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}
