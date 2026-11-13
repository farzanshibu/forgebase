package provider

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/boolplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	_ resource.Resource                   = (*collectionResource)(nil)
	_ resource.ResourceWithConfigure      = (*collectionResource)(nil)
	_ resource.ResourceWithImportState    = (*collectionResource)(nil)
	_ resource.ResourceWithValidateConfig = (*collectionResource)(nil)
)

var (
	nameRE      = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9_]{0,62}$`)
	nameRENote  = "1-63 letters, digits or underscores, starting with a letter"
	reservedFld = map[string]bool{"id": true, "created": true, "updated": true, "_version": true, "expand": true, "rowid": true, "oid": true}
)

// NewCollectionResource returns the forgebase_collection resource.
func NewCollectionResource() resource.Resource { return &collectionResource{} }

type collectionResource struct{ base }

type collectionModel struct {
	ID           types.String `tfsdk:"id"`
	Name         types.String `tfsdk:"name"`
	Type         types.String `tfsdk:"type"`
	SchemaMode   types.String `tfsdk:"schema_mode"`
	Versioned    types.Bool   `tfsdk:"versioned"`
	TenantScoped types.Bool   `tfsdk:"tenant_scoped"`
	ViewQuery    types.String `tfsdk:"view_query"`
	Fields       types.List   `tfsdk:"fields"`
	Version      types.Int64  `tfsdk:"version"`
	Created      types.String `tfsdk:"created"`
	Updated      types.String `tfsdk:"updated"`
}

type fieldModel struct {
	ID          types.String         `tfsdk:"id"`
	Name        types.String         `tfsdk:"name"`
	Type        types.String         `tfsdk:"type"`
	Required    types.Bool           `tfsdk:"required"`
	Unique      types.Bool           `tfsdk:"unique"`
	Hidden      types.Bool           `tfsdk:"hidden"`
	Default     jsontypes.Normalized `tfsdk:"default"`
	Options     jsontypes.Normalized `tfsdk:"options"`
	RenamedFrom types.String         `tfsdk:"renamed_from"`
}

var fieldAttrTypes = map[string]attr.Type{
	"id": types.StringType, "name": types.StringType, "type": types.StringType,
	"required": types.BoolType, "unique": types.BoolType, "hidden": types.BoolType,
	"default": jsontypes.NormalizedType{}, "options": jsontypes.NormalizedType{}, "renamed_from": types.StringType,
}

var fieldObjType = types.ObjectType{AttrTypes: fieldAttrTypes}

func (r *collectionResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_collection"
}

func (r *collectionResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *collectionResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "A ForgeBase collection: a typed table (`base`), a user table (`auth`) or a read-only SQL view (`view`). " +
			"Changing `fields` applies transactional DDL on the server and is refused when existing data does not fit.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{
				Computed:      true,
				Description:   "Server-assigned collection id.",
				PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()},
			},
			"name": schema.StringAttribute{
				Required:    true,
				Description: "Collection name (" + nameRENote + "). Renaming is applied in place.",
				Validators:  []validator.String{stringvalidator.RegexMatches(nameRE, "must be "+nameRENote)},
			},
			"type": schema.StringAttribute{
				Optional: true, Computed: true,
				Default:       stringdefault.StaticString("base"),
				Description:   "`base` (default), `auth` (adds the account fields) or `view` (read-only, needs `view_query`). Cannot change after creation.",
				Validators:    []validator.String{stringvalidator.OneOf("base", "auth", "view")},
				PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()},
			},
			"schema_mode": schema.StringAttribute{
				Optional: true, Computed: true,
				Default:     stringdefault.StaticString("strict"),
				Description: "`strict` (default) accepts declared fields only; `flexible` keeps other top-level keys as a JSON document.",
				Validators:  []validator.String{stringvalidator.OneOf("strict", "flexible")},
			},
			"versioned": schema.BoolAttribute{
				Optional: true, Computed: true,
				Default:     booldefault.StaticBool(false),
				Description: "Keep every version of every record.",
			},
			"tenant_scoped": schema.BoolAttribute{
				Optional: true, Computed: true,
				Default:       booldefault.StaticBool(false),
				Description:   "Rows belong to tenants (environments) and are isolated per tenant. Cannot change after creation.",
				PlanModifiers: []planmodifier.Bool{boolplanmodifier.RequiresReplace()},
			},
			"view_query": schema.StringAttribute{
				Optional:    true,
				Description: "The single `SELECT` statement of a `view` collection. Not allowed for other types.",
			},
			"fields": schema.ListNestedAttribute{
				Optional: true, Computed: true,
				Description: "Declared fields in order. Omit for `view` collections (their fields come from the query). " +
					"Auth collections get their system fields (email, password, …) automatically; declare one only to tune it.",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"id": schema.StringAttribute{Computed: true, Description: "Stable field id; kept across renames."},
					"name": schema.StringAttribute{
						Required:    true,
						Description: "Field name (" + nameRENote + "; `id`, `created`, `updated`, `expand` and names starting with `_` are reserved).",
						Validators:  []validator.String{stringvalidator.RegexMatches(nameRE, "must be "+nameRENote)},
					},
					"type": schema.StringAttribute{
						Required:    true,
						Description: "One of: " + strings.Join(fieldTypes, ", ") + ".",
						Validators:  []validator.String{stringvalidator.OneOf(fieldTypes...)},
					},
					"required": schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "A value is required."},
					"unique":   schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "Values are unique."},
					"hidden":   schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "Never returned by the API to non-privileged callers."},
					"default": schema.StringAttribute{
						Optional: true, CustomType: jsontypes.NormalizedType{},
						Description: "Default value as a JSON document, e.g. `jsonencode(false)` or `jsonencode(\"draft\")`.",
					},
					"options": schema.StringAttribute{
						Optional: true, CustomType: jsontypes.NormalizedType{},
						Description: "Type-specific options as a JSON object, e.g. `jsonencode({ max_length = 200 })` for text, " +
							"`{ values = [\"a\",\"b\"], max_select = 1 }` for select, `{ collection_id = \"…\" }` for relation, `{ dimensions = 384 }` for vector. " +
							"Only the keys you set are tracked, so server defaults do not show as drift.",
					},
					"renamed_from": schema.StringAttribute{
						Optional:    true,
						Description: "Previous name of this field. Set it in the same change that renames the field to keep the field's data (the server matches by id, and this tells the provider which existing field it is).",
					},
				}},
			},
			"version": schema.Int64Attribute{Computed: true, Description: "Schema version; increases with every change."},
			"created": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creation time."},
			"updated": schema.StringAttribute{Computed: true, Description: "Last schema change."},
		},
	}
}

func (r *collectionResource) ValidateConfig(ctx context.Context, req resource.ValidateConfigRequest, resp *resource.ValidateConfigResponse) {
	var m collectionModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	typ := "base"
	if !m.Type.IsNull() && !m.Type.IsUnknown() {
		typ = m.Type.ValueString()
	}
	if !m.Type.IsUnknown() && !m.ViewQuery.IsUnknown() {
		hasQ := !m.ViewQuery.IsNull() && strings.TrimSpace(m.ViewQuery.ValueString()) != ""
		switch {
		case typ == "view" && !hasQ:
			resp.Diagnostics.AddAttributeError(path.Root("view_query"), "Missing view query", "A `view` collection needs `view_query`.")
		case typ != "view" && !m.ViewQuery.IsNull():
			resp.Diagnostics.AddAttributeError(path.Root("view_query"), "Unexpected view query", "Only `view` collections have a `view_query`.")
		}
	}
	if m.Fields.IsNull() || m.Fields.IsUnknown() {
		return
	}
	if typ == "view" && len(m.Fields.Elements()) > 0 {
		resp.Diagnostics.AddAttributeError(path.Root("fields"), "Fields on a view", "View fields are derived from the query; do not declare them.")
	}
	if len(m.Fields.Elements()) > 500 {
		resp.Diagnostics.AddAttributeError(path.Root("fields"), "Too many fields", "A collection has at most 500 fields.")
	}
	seen := map[string]bool{}
	for i, el := range m.Fields.Elements() {
		obj, ok := el.(types.Object)
		if !ok || obj.IsUnknown() || obj.IsNull() {
			continue
		}
		var f fieldModel
		if d := obj.As(ctx, &f, basicObjectOptions); d.HasError() {
			continue
		}
		p := path.Root("fields").AtListIndex(i)
		if !f.Name.IsUnknown() && !f.Name.IsNull() {
			n := strings.ToLower(f.Name.ValueString())
			if reservedFld[n] || strings.HasPrefix(n, "_") {
				resp.Diagnostics.AddAttributeError(p.AtName("name"), "Reserved field name", fmt.Sprintf("%q is reserved by the server.", f.Name.ValueString()))
			}
			if seen[n] {
				resp.Diagnostics.AddAttributeError(p.AtName("name"), "Duplicate field name", fmt.Sprintf("Field %q is declared twice (names are case-insensitive).", f.Name.ValueString()))
			}
			seen[n] = true
		}
		if !f.Default.IsNull() && !f.Default.IsUnknown() && !json.Valid([]byte(f.Default.ValueString())) {
			resp.Diagnostics.AddAttributeError(p.AtName("default"), "Invalid JSON", "`default` must be a JSON document.")
		}
		if f.Type.IsUnknown() || f.Type.IsNull() || f.Options.IsUnknown() {
			continue
		}
		var opts map[string]any
		if !f.Options.IsNull() {
			if err := json.Unmarshal([]byte(f.Options.ValueString()), &opts); err != nil {
				resp.Diagnostics.AddAttributeError(p.AtName("options"), "Invalid options", "`options` must be a JSON object.")
				continue
			}
		}
		for _, msg := range validateFieldOptions(f.Type.ValueString(), opts, f.Unique.ValueBool(), !f.Default.IsNull()) {
			resp.Diagnostics.AddAttributeError(p.AtName("options"), "Invalid field options", fmt.Sprintf("Field %q: %s", f.Name.ValueString(), msg))
		}
	}
}

// validateFieldOptions mirrors the server's field-option checks so mistakes
// show at plan time.
func validateFieldOptions(typ string, o map[string]any, unique, hasDefault bool) []string {
	var bad []string
	num := func(k string) (float64, bool) { v, ok := o[k].(float64); return v, ok }
	switch typ {
	case "text":
		a, aok := num("min_length")
		b, bok := num("max_length")
		if aok && bok && a > b {
			bad = append(bad, "min_length is greater than max_length")
		}
	case "number":
		a, aok := num("min")
		b, bok := num("max")
		if aok && bok && a > b {
			bad = append(bad, "min is greater than max")
		}
	case "select":
		vals, _ := o["values"].([]any)
		if len(vals) == 0 {
			bad = append(bad, "select needs a non-empty `values` list")
			break
		}
		seen := map[string]bool{}
		for _, v := range vals {
			s, _ := v.(string)
			if s == "" || len(s) > 255 || seen[s] {
				bad = append(bad, "select values must be unique, non-empty and at most 255 bytes")
				break
			}
			seen[s] = true
		}
		if ms, ok := num("max_select"); ok && (ms < 1 || int(ms) > len(vals)) {
			bad = append(bad, "max_select must be between 1 and the number of values")
		}
	case "array":
		a, aok := num("min_items")
		b, bok := num("max_items")
		if aok && bok && a > b {
			bad = append(bad, "min_items is greater than max_items")
		}
	case "relation":
		if s, _ := o["collection_id"].(string); s == "" {
			bad = append(bad, "relation needs `collection_id` (the target collection's id)")
		}
		if ms, ok := num("max_select"); ok && ms < 1 {
			bad = append(bad, "max_select must be at least 1")
		}
	case "file":
		if ms, ok := num("max_select"); ok && ms < 1 {
			bad = append(bad, "max_select must be at least 1")
		}
	case "password":
		if ml, ok := num("min_length"); ok && (ml < 1 || ml > 256) {
			bad = append(bad, "min_length must be between 1 and 256")
		}
		if unique || hasDefault {
			bad = append(bad, "password fields cannot be unique or have a default")
		}
	case "vector":
		d, ok := num("dimensions")
		if !ok || d < 1 || d > 16000 {
			bad = append(bad, "vector needs `dimensions` between 1 and 16000")
		}
		if unique {
			bad = append(bad, "vector fields cannot be unique")
		}
	case "json", "geo":
		if unique {
			bad = append(bad, "json and geo fields cannot be unique")
		}
	}
	return bad
}

func (r *collectionResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	// The import id is the collection name or id.
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

func (r *collectionResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan collectionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body, planFields, diags := plan.toAPI(ctx, nil)
	resp.Diagnostics.Append(diags...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiCollection
	if err := r.c.Post(ctx, "/api/v1/collections", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating collection", err)
		return
	}
	state, d := collectionState(ctx, out, planFields, !plan.Fields.IsUnknown())
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, state)...)
}

func (r *collectionResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior collectionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiCollection
	err := r.c.Get(ctx, "/api/v1/collections/"+seg(prior.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "collection", err) {
		return
	}
	var priorFields []fieldModel
	if !prior.Fields.IsNull() && !prior.Fields.IsUnknown() {
		resp.Diagnostics.Append(prior.Fields.ElementsAs(ctx, &priorFields, false)...)
	}
	state, d := collectionState(ctx, out, priorFields, !prior.Fields.IsNull())
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, state)...)
}

func (r *collectionResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, prior collectionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	id := prior.ID.ValueString()
	var current apiCollection
	if err := r.c.Get(ctx, "/api/v1/collections/"+seg(id), nil, &current); err != nil {
		addErr(&resp.Diagnostics, "Error reading collection before update", err)
		return
	}
	body, planFields, diags := plan.toAPI(ctx, &current)
	resp.Diagnostics.Append(diags...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiCollection
	if err := r.c.Put(ctx, "/api/v1/collections/"+seg(id), body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating collection", err)
		return
	}
	state, d := collectionState(ctx, out, planFields, !plan.Fields.IsUnknown())
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, state)...)
}

func (r *collectionResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state collectionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/collections/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting collection", err)
	}
}

// toAPI builds the request body. current (on update) supplies field ids and
// the system fields the server insists on keeping.
func (m collectionModel) toAPI(ctx context.Context, current *apiCollection) (apiCollection, []fieldModel, diagList) {
	var diags diagList
	body := apiCollection{
		Name:       m.Name.ValueString(),
		Type:       m.Type.ValueString(),
		SchemaMode: m.SchemaMode.ValueString(),
		Versioned:  m.Versioned.ValueBool(),
		Fields:     []apiField{},
	}
	if m.TenantScoped.ValueBool() {
		body.Options = map[string]json.RawMessage{"tenant_scoped": json.RawMessage("true")}
	}
	if !m.ViewQuery.IsNull() && !m.ViewQuery.IsUnknown() {
		q := m.ViewQuery.ValueString()
		body.ViewQuery = &q
	}
	var pf []fieldModel
	if !m.Fields.IsNull() && !m.Fields.IsUnknown() {
		diags.Append(m.Fields.ElementsAs(ctx, &pf, false)...)
	}
	byName := map[string]apiField{}
	if current != nil {
		for _, f := range current.Fields {
			byName[strings.ToLower(f.Name)] = f
		}
	}
	used := map[string]bool{}
	for _, f := range pf {
		af := apiField{
			Name: f.Name.ValueString(), Type: f.Type.ValueString(),
			Required: f.Required.ValueBool(), Unique: f.Unique.ValueBool(), Hidden: f.Hidden.ValueBool(),
			Default: jsonRaw(f.Default), Extra: map[string]json.RawMessage{},
		}
		if cur, ok := byName[strings.ToLower(af.Name)]; ok {
			af.ID, af.System = cur.ID, cur.System
			used[cur.ID] = true
		} else if rf := f.RenamedFrom.ValueString(); rf != "" {
			if cur, ok := byName[strings.ToLower(rf)]; ok && !used[cur.ID] {
				af.ID, af.System = cur.ID, cur.System
				used[cur.ID] = true
			}
		}
		if raw := jsonRaw(f.Options); raw != nil {
			var o map[string]json.RawMessage
			if err := json.Unmarshal(raw, &o); err != nil {
				diags.AddError("Invalid field options", fmt.Sprintf("Field %q: options must be a JSON object: %s", af.Name, err))
			}
			for k, v := range o {
				af.Extra[k] = v
			}
		}
		body.Fields = append(body.Fields, af)
	}
	if current != nil {
		// System fields must stay (the server refuses to drop them).
		for _, cur := range current.Fields {
			if cur.System && !used[cur.ID] {
				body.Fields = append(body.Fields, cur)
			}
		}
	}
	return body, pf, diags
}

// collectionState maps the API collection to the Terraform model. hint holds
// the previous or planned fields: it decides which options keys are tracked,
// which system fields are shown (when strict), and keeps `default`/`renamed_from` stable.
func collectionState(ctx context.Context, c apiCollection, hint []fieldModel, strict bool) (collectionModel, diagList) {
	var diags diagList
	m := collectionModel{
		ID:           types.StringValue(c.ID),
		Name:         types.StringValue(c.Name),
		Type:         types.StringValue(c.Type),
		SchemaMode:   types.StringValue(c.SchemaMode),
		Versioned:    types.BoolValue(c.Versioned),
		TenantScoped: types.BoolValue(c.tenantScoped()),
		ViewQuery:    nullableString(c.ViewQuery),
		Version:      types.Int64Value(c.Version),
		Created:      types.StringValue(c.Created),
		Updated:      types.StringValue(c.Updated),
	}
	fields := fieldsToModels(c.Fields, hint, strict)
	l, d := types.ListValueFrom(ctx, fieldObjType, fields)
	diags.Append(d...)
	m.Fields = l
	return m, diags
}

// fieldsToModels converts API fields. With strict == false every option key and
// system field is kept (import, data sources, omitted `fields`).
func fieldsToModels(api []apiField, hint []fieldModel, strict bool) []fieldModel {
	byName := map[string]fieldModel{}
	for _, h := range hint {
		byName[strings.ToLower(h.Name.ValueString())] = h
	}
	out := make([]fieldModel, 0, len(api))
	for _, f := range api {
		h, declared := byName[strings.ToLower(f.Name)]
		if f.System && strict && !declared {
			continue
		}
		fm := fieldModel{
			ID:          types.StringValue(f.ID),
			Name:        types.StringValue(f.Name),
			Type:        types.StringValue(f.Type),
			Required:    types.BoolValue(f.Required),
			Unique:      types.BoolValue(f.Unique),
			Hidden:      types.BoolValue(f.Hidden),
			Default:     jsonValue(f.Default),
			RenamedFrom: types.StringNull(),
		}
		if declared {
			fm.RenamedFrom = h.RenamedFrom
			if !h.Default.IsNull() && !h.Default.IsUnknown() && jsonEqual([]byte(h.Default.ValueString()), f.Default) {
				fm.Default = h.Default
			}
		}
		fm.Options = trackedOptions(f.Extra, h.Options, strict && declared)
		out = append(out, fm)
	}
	return out
}

// trackedOptions returns the type options as a JSON object limited to the keys
// the configuration declares (server defaults would otherwise look like drift).
// Without a declared counterpart every key is returned.
func trackedOptions(extra map[string]json.RawMessage, declared jsontypes.Normalized, hasDeclared bool) jsontypes.Normalized {
	if len(extra) == 0 {
		return jsontypes.NewNormalizedNull()
	}
	keep := extra
	if hasDeclared {
		if declared.IsNull() || declared.IsUnknown() {
			return jsontypes.NewNormalizedNull()
		}
		var want map[string]json.RawMessage
		if json.Unmarshal([]byte(declared.ValueString()), &want) != nil {
			return jsontypes.NewNormalizedNull()
		}
		keep = map[string]json.RawMessage{}
		for k := range want {
			if v, ok := extra[k]; ok {
				keep[k] = v
			}
		}
		// Keep the declared text when the value is semantically unchanged.
		b, _ := json.Marshal(keep)
		if jsonEqual(b, []byte(declared.ValueString())) {
			return declared
		}
	}
	if len(keep) == 0 {
		return jsontypes.NewNormalizedNull()
	}
	b, _ := json.Marshal(keep)
	return jsontypes.NewNormalizedValue(string(b))
}
