package provider_test

import (
	"context"
	"strings"
	"testing"

	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/provider"
	"github.com/hashicorp/terraform-plugin-framework/resource"

	fb "github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/provider"
)

// Every schema must pass the framework's implementation checks (defaults on
// computed attributes, plan modifiers, nested-attribute rules, …).
func TestSchemasAreValid(t *testing.T) {
	ctx := context.Background()
	p := fb.New("test")()

	var pr provider.SchemaResponse
	p.Schema(ctx, provider.SchemaRequest{}, &pr)
	if d := pr.Schema.ValidateImplementation(ctx); d.HasError() {
		t.Fatalf("provider schema: %v", d)
	}
	var mr provider.MetadataResponse
	p.Metadata(ctx, provider.MetadataRequest{}, &mr)

	names := map[string]bool{}
	for _, f := range p.Resources(ctx) {
		r := f()
		var meta resource.MetadataResponse
		r.Metadata(ctx, resource.MetadataRequest{ProviderTypeName: mr.TypeName}, &meta)
		var sr resource.SchemaResponse
		r.Schema(ctx, resource.SchemaRequest{}, &sr)
		if d := sr.Schema.ValidateImplementation(ctx); d.HasError() {
			t.Errorf("%s: %v", meta.TypeName, d)
		}
		if !strings.HasPrefix(meta.TypeName, "forgebase_") || names[meta.TypeName] {
			t.Errorf("bad or duplicate resource name %q", meta.TypeName)
		}
		names[meta.TypeName] = true
		for n, a := range sr.Schema.Attributes {
			if a.GetDescription() == "" && a.GetMarkdownDescription() == "" {
				t.Errorf("%s.%s has no description", meta.TypeName, n)
			}
		}
		if sr.Schema.Description == "" {
			t.Errorf("%s has no description", meta.TypeName)
		}
		if _, ok := r.(resource.ResourceWithImportState); !ok {
			t.Errorf("%s does not support import", meta.TypeName)
		}
	}
	for _, f := range p.DataSources(ctx) {
		d := f()
		var meta datasource.MetadataResponse
		d.Metadata(ctx, datasource.MetadataRequest{ProviderTypeName: mr.TypeName}, &meta)
		var sr datasource.SchemaResponse
		d.Schema(ctx, datasource.SchemaRequest{}, &sr)
		if dd := sr.Schema.ValidateImplementation(ctx); dd.HasError() {
			t.Errorf("%s: %v", meta.TypeName, dd)
		}
		for n, a := range sr.Schema.Attributes {
			if a.GetDescription() == "" && a.GetMarkdownDescription() == "" {
				t.Errorf("%s.%s has no description", meta.TypeName, n)
			}
		}
	}
	if len(names) < 16 {
		t.Errorf("expected at least 16 resources, got %d", len(names))
	}
}
