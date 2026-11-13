// Package docgen renders Terraform Registry style documentation (the layout
// tfplugindocs produces) from the provider's schemas and the examples/ tree.
package docgen

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
	"github.com/hashicorp/terraform-plugin-go/tftypes"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/provider"
)

const providerName = "forgebase"

// Generate returns the documentation files (paths relative to docs/) for the
// provider, reading examples from examplesDir.
func Generate(examplesDir string) (map[string]string, error) {
	srv := providerserver.NewProtocol6(provider.New("docs")())()
	resp, err := srv.GetProviderSchema(context.Background(), &tfprotov6.GetProviderSchemaRequest{})
	if err != nil {
		return nil, err
	}
	for _, d := range resp.Diagnostics {
		if d.Severity == tfprotov6.DiagnosticSeverityError {
			return nil, fmt.Errorf("%s: %s", d.Summary, d.Detail)
		}
	}
	files := map[string]string{}
	files["index.md"] = renderProvider(resp.Provider, examplesDir)
	for name, s := range resp.ResourceSchemas {
		files[filepath.Join("resources", strings.TrimPrefix(name, providerName+"_")+".md")] = renderObject(name, "Resource", s, examplesDir,
			"resources", "resource.tf", "import.sh")
	}
	for name, s := range resp.DataSourceSchemas {
		files[filepath.Join("data-sources", strings.TrimPrefix(name, providerName+"_")+".md")] = renderObject(name, "Data Source", s, examplesDir,
			"data-sources", "data-source.tf", "")
	}
	return files, nil
}

// Write generates the docs into docsDir, removing stale generated pages.
func Write(docsDir, examplesDir string) error {
	files, err := Generate(examplesDir)
	if err != nil {
		return err
	}
	for _, sub := range []string{"resources", "data-sources"} {
		old, _ := filepath.Glob(filepath.Join(docsDir, sub, "*.md"))
		for _, f := range old {
			if _, keep := files[filepath.Join(sub, filepath.Base(f))]; !keep {
				_ = os.Remove(f)
			}
		}
	}
	for rel, content := range files {
		p := filepath.Join(docsDir, rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
			return err
		}
	}
	return nil
}

func readExample(dir ...string) string {
	b, err := os.ReadFile(filepath.Join(dir...))
	if err != nil {
		return ""
	}
	return strings.TrimRight(string(b), "\n")
}

func renderProvider(s *tfprotov6.Schema, examplesDir string) string {
	var b strings.Builder
	desc := "Manage ForgeBase configuration (collections, storage, queues, functions, identity and more) through its REST API."
	fmt.Fprintf(&b, "---\npage_title: \"Provider: ForgeBase\"\nsubcategory: \"\"\ndescription: |-\n  %s\n---\n\n", desc)
	b.WriteString("# ForgeBase Provider\n\n")
	b.WriteString(desc + "\n\n")
	b.WriteString("The provider talks to the server's REST API (`/api/v1`, `/api/auth`) with one credential: a service-account API key (`fbk_…`), " +
		"a personal access token (`fbp_…`) or an access token. Most resources need an administrator credential; each resource page notes what it requires.\n\n")
	if ex := readExample(examplesDir, "provider", "provider.tf"); ex != "" {
		b.WriteString("## Example Usage\n\n```terraform\n" + ex + "\n```\n\n")
	}
	b.WriteString("## Authentication and tenancy\n\n")
	b.WriteString("- `url` and `token` default to the `FORGEBASE_URL` and `FORGEBASE_TOKEN` environment variables, so no secret has to appear in configuration.\n")
	b.WriteString("- `realm`, `org` and `tenant` set the `X-Forgebase-Realm`, `X-Forgebase-Org` and `X-Forgebase-Tenant` headers. Set `tenant` to an environment id " +
		"(see `forgebase_project`) to manage that environment's collections, buckets and queues.\n")
	b.WriteString("- Credentials that carry scopes (`<area>:read|write`) need `write` on every area the configuration touches, and `read` for refresh.\n\n")
	b.WriteString(renderSchema(s))
	return b.String()
}

func renderObject(name, kind string, s *tfprotov6.Schema, examplesDir, group, exampleFile, importFile string) string {
	short := strings.TrimPrefix(name, providerName+"_")
	desc := s.Block.Description
	var b strings.Builder
	fmt.Fprintf(&b, "---\npage_title: %q\nsubcategory: \"\"\ndescription: |-\n  %s\n---\n\n", name+" "+kind+" - terraform-provider-forgebase", indent(desc, "  "))
	fmt.Fprintf(&b, "# %s (%s)\n\n%s\n\n", name, kind, desc)
	if ex := readExample(examplesDir, group, name, exampleFile); ex != "" {
		b.WriteString("## Example Usage\n\n```terraform\n" + ex + "\n```\n\n")
	}
	b.WriteString(renderSchema(s))
	if importFile != "" {
		if imp := readExample(examplesDir, group, name, importFile); imp != "" {
			b.WriteString("\n## Import\n\nImport is supported using the following syntax:\n\n```shell\n" + imp + "\n```\n")
		}
	}
	_ = short
	return b.String()
}

func indent(s, pad string) string {
	return strings.ReplaceAll(strings.TrimSpace(s), "\n", "\n"+pad)
}

func renderSchema(s *tfprotov6.Schema) string {
	var b strings.Builder
	b.WriteString("<!-- schema generated by internal/docgen; do not edit by hand (run `go generate ./...`) -->\n## Schema\n\n")
	var nested []nestedDoc
	writeAttrs(&b, s.Block.Attributes, nil, &nested)
	for i := 0; i < len(nested); i++ {
		n := nested[i]
		fmt.Fprintf(&b, "\n<a id=\"nestedatt--%s\"></a>\n### Nested Schema for `%s`\n\n", strings.Join(n.path, "--"), strings.Join(n.path, "."))
		writeAttrs(&b, n.attrs, n.path, &nested)
	}
	return b.String()
}

type nestedDoc struct {
	path  []string
	attrs []*tfprotov6.SchemaAttribute
}

func writeAttrs(b *strings.Builder, attrs []*tfprotov6.SchemaAttribute, parent []string, nested *[]nestedDoc) {
	var req, opt, ro []*tfprotov6.SchemaAttribute
	for _, a := range attrs {
		switch {
		case a.Required:
			req = append(req, a)
		case a.Optional:
			opt = append(opt, a)
		default:
			ro = append(ro, a)
		}
	}
	for _, g := range []struct {
		title string
		list  []*tfprotov6.SchemaAttribute
	}{{"Required", req}, {"Optional", opt}, {"Read-Only", ro}} {
		if len(g.list) == 0 {
			continue
		}
		sort.Slice(g.list, func(i, j int) bool { return g.list[i].Name < g.list[j].Name })
		fmt.Fprintf(b, "### %s\n\n", g.title)
		for _, a := range g.list {
			t := typeName(a)
			if a.Sensitive {
				t += ", Sensitive"
			}
			line := fmt.Sprintf("- `%s` (%s)", a.Name, t)
			if a.Description != "" {
				line += " " + strings.ReplaceAll(strings.TrimSpace(a.Description), "\n", " ")
			}
			if a.NestedType != nil {
				p := append(append([]string{}, parent...), a.Name)
				line += fmt.Sprintf(" (see [below for nested schema](#nestedatt--%s))", strings.Join(p, "--"))
				*nested = append(*nested, nestedDoc{path: p, attrs: a.NestedType.Attributes})
			}
			b.WriteString(line + "\n")
		}
		b.WriteString("\n")
	}
}

func typeName(a *tfprotov6.SchemaAttribute) string {
	if a.NestedType != nil {
		switch a.NestedType.Nesting {
		case tfprotov6.SchemaObjectNestingModeList:
			return "Attributes List"
		case tfprotov6.SchemaObjectNestingModeSet:
			return "Attributes Set"
		case tfprotov6.SchemaObjectNestingModeMap:
			return "Attributes Map"
		default:
			return "Attributes"
		}
	}
	return tfTypeName(a.Type)
}

func tfTypeName(t tftypes.Type) string {
	switch {
	case t.Is(tftypes.String):
		return "String"
	case t.Is(tftypes.Number):
		return "Number"
	case t.Is(tftypes.Bool):
		return "Boolean"
	}
	switch tt := t.(type) {
	case tftypes.List:
		return "List of " + tfTypeName(tt.ElementType)
	case tftypes.Set:
		return "Set of " + tfTypeName(tt.ElementType)
	case tftypes.Map:
		return "Map of " + tfTypeName(tt.ElementType)
	case tftypes.Object:
		return "Object"
	}
	return "Dynamic"
}
