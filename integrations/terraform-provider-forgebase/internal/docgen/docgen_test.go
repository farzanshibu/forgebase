package docgen_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/docgen"
)

// The committed docs must match the schemas and examples: run
// `go generate ./...` after changing either.
func TestDocsAreUpToDate(t *testing.T) {
	root := filepath.Join("..", "..")
	files, err := docgen.Generate(filepath.Join(root, "examples"))
	if err != nil {
		t.Fatal(err)
	}
	if len(files) < 25 {
		t.Fatalf("only %d pages generated", len(files))
	}
	for rel, want := range files {
		got, err := os.ReadFile(filepath.Join(root, "docs", rel))
		if err != nil {
			t.Errorf("docs/%s is missing: run `go generate ./...`", rel)
			continue
		}
		if string(got) != want {
			t.Errorf("docs/%s is stale: run `go generate ./...`", rel)
		}
	}
	// No page may be left behind for a removed resource.
	for _, sub := range []string{"resources", "data-sources"} {
		onDisk, _ := filepath.Glob(filepath.Join(root, "docs", sub, "*.md"))
		for _, f := range onDisk {
			if _, ok := files[filepath.Join(sub, filepath.Base(f))]; !ok {
				t.Errorf("%s has no matching resource: run `go generate ./...`", f)
			}
		}
	}
}

// Every resource page documents an example and, for resources, an import.
func TestExamplesExist(t *testing.T) {
	files, err := docgen.Generate(filepath.Join("..", "..", "examples"))
	if err != nil {
		t.Fatal(err)
	}
	for rel, content := range files {
		if rel == "index.md" {
			continue
		}
		if !contains(content, "## Example Usage") {
			t.Errorf("%s has no example", rel)
		}
		if filepath.Dir(rel) == "resources" && !contains(content, "## Import") {
			t.Errorf("%s has no import section", rel)
		}
	}
}

func contains(s, sub string) bool {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}
