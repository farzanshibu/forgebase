package provider

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/diag"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/types"
	"github.com/hashicorp/terraform-plugin-framework/types/basetypes"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/client"
)

type diagList = diag.Diagnostics

var basicObjectOptions = basetypes.ObjectAsOptions{}

func seg(s string) string { return client.Seg(s) }

func isGone(err error) bool { return client.IsNotFound(err) }

func isForbidden(err error) bool { return client.StatusOf(err) == 403 }

// base holds the API client every resource and data source needs.
type base struct{ c *client.Client }

func (b *base) configureResource(req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	if req.ProviderData == nil {
		return
	}
	c, ok := req.ProviderData.(*client.Client)
	if !ok {
		resp.Diagnostics.AddError("Unexpected provider data", fmt.Sprintf("expected *client.Client, got %T", req.ProviderData))
		return
	}
	b.c = c
}

func (b *base) configureDataSource(req datasource.ConfigureRequest, resp *datasource.ConfigureResponse) {
	if req.ProviderData == nil {
		return
	}
	c, ok := req.ProviderData.(*client.Client)
	if !ok {
		resp.Diagnostics.AddError("Unexpected provider data", fmt.Sprintf("expected *client.Client, got %T", req.ProviderData))
		return
	}
	b.c = c
}

// addErr reports an API or client error with a short summary.
func addErr(d *diag.Diagnostics, summary string, err error) {
	d.AddError(summary, err.Error())
}

// ---- conversions -------------------------------------------------------------

func strList(items []string) types.List {
	vals := make([]attr.Value, len(items))
	for i, s := range items {
		vals[i] = types.StringValue(s)
	}
	return types.ListValueMust(types.StringType, vals)
}

func strSet(items []string) types.Set {
	vals := make([]attr.Value, len(items))
	for i, s := range items {
		vals[i] = types.StringValue(s)
	}
	return types.SetValueMust(types.StringType, vals)
}

func strMap(m map[string]string) types.Map {
	vals := make(map[string]attr.Value, len(m))
	for k, v := range m {
		vals[k] = types.StringValue(v)
	}
	return types.MapValueMust(types.StringType, vals)
}

func listStrings(l types.List) []string {
	if l.IsNull() || l.IsUnknown() {
		return nil
	}
	out := make([]string, 0, len(l.Elements()))
	for _, e := range l.Elements() {
		if s, ok := e.(types.String); ok {
			out = append(out, s.ValueString())
		}
	}
	return out
}

func setStrings(s types.Set) []string {
	if s.IsNull() || s.IsUnknown() {
		return nil
	}
	out := make([]string, 0, len(s.Elements()))
	for _, e := range s.Elements() {
		if v, ok := e.(types.String); ok {
			out = append(out, v.ValueString())
		}
	}
	sort.Strings(out)
	return out
}

func mapStrings(m types.Map) map[string]string {
	if m.IsNull() || m.IsUnknown() {
		return nil
	}
	out := make(map[string]string, len(m.Elements()))
	for k, e := range m.Elements() {
		if v, ok := e.(types.String); ok {
			out[k] = v.ValueString()
		}
	}
	return out
}

func strPtr(s types.String) *string {
	if s.IsNull() || s.IsUnknown() {
		return nil
	}
	v := s.ValueString()
	return &v
}

func int64Ptr(i types.Int64) *int64 {
	if i.IsNull() || i.IsUnknown() {
		return nil
	}
	v := i.ValueInt64()
	return &v
}

func nullableString(s *string) types.String {
	if s == nil {
		return types.StringNull()
	}
	return types.StringValue(*s)
}

func nullableInt64(i *int64) types.Int64 {
	if i == nil {
		return types.Int64Null()
	}
	return types.Int64Value(*i)
}

// jsonValue converts raw JSON to a normalized JSON string value (null for
// absent or JSON null).
func jsonValue(raw json.RawMessage) jsontypes.Normalized {
	if len(raw) == 0 || string(raw) == "null" {
		return jsontypes.NewNormalizedNull()
	}
	return jsontypes.NewNormalizedValue(string(raw))
}

// jsonRaw returns the JSON of a normalized value, or nil when null/unknown.
func jsonRaw(v jsontypes.Normalized) json.RawMessage {
	if v.IsNull() || v.IsUnknown() || v.ValueString() == "" {
		return nil
	}
	return json.RawMessage(v.ValueString())
}

// anyJSON marshals v into a normalized JSON value ("null" when v is nil).
func anyJSON(v any) jsontypes.Normalized {
	if v == nil {
		return jsontypes.NewNormalizedNull()
	}
	b, err := json.Marshal(v)
	if err != nil || string(b) == "null" {
		return jsontypes.NewNormalizedNull()
	}
	return jsontypes.NewNormalizedValue(string(b))
}

// items decodes the `{"items": [...]}` list envelope.
type items[T any] struct {
	Items []T `json:"items"`
}

// removeIfGone removes the resource from state when err is a 404 and reports
// whether it did. Other errors become diagnostics.
func removeIfGone(ctx context.Context, state interface{ RemoveResource(context.Context) }, d *diag.Diagnostics, what string, err error) bool {
	if err == nil {
		return false
	}
	if client.IsNotFound(err) {
		state.RemoveResource(ctx)
		return true
	}
	addErr(d, "Error reading "+what, err)
	return true
}
