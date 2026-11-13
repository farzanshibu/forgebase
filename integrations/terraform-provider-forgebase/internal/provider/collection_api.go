package provider

import (
	"bytes"
	"encoding/json"
	"reflect"
	"sort"
)

// apiField is one field of a collection. The server flattens type-specific
// options (`max_length`, `values`, …) beside the common keys, so they are kept
// in Extra and merged back when encoding.
type apiField struct {
	ID       string
	Name     string
	Type     string
	Required bool
	Unique   bool
	Hidden   bool
	System   bool
	Default  json.RawMessage
	Extra    map[string]json.RawMessage
}

var fieldCommonKeys = map[string]bool{
	"id": true, "name": true, "type": true, "required": true, "unique": true, "hidden": true, "system": true, "default": true,
}

func (f *apiField) UnmarshalJSON(data []byte) error {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	*f = apiField{Extra: map[string]json.RawMessage{}}
	for k, v := range raw {
		switch k {
		case "id":
			_ = json.Unmarshal(v, &f.ID)
		case "name":
			_ = json.Unmarshal(v, &f.Name)
		case "type":
			_ = json.Unmarshal(v, &f.Type)
		case "required":
			_ = json.Unmarshal(v, &f.Required)
		case "unique":
			_ = json.Unmarshal(v, &f.Unique)
		case "hidden":
			_ = json.Unmarshal(v, &f.Hidden)
		case "system":
			_ = json.Unmarshal(v, &f.System)
		case "default":
			if string(v) != "null" {
				f.Default = v
			}
		default:
			f.Extra[k] = v
		}
	}
	return nil
}

func (f apiField) MarshalJSON() ([]byte, error) {
	m := map[string]any{}
	for k, v := range f.Extra {
		if !fieldCommonKeys[k] {
			m[k] = v
		}
	}
	if f.ID != "" {
		m["id"] = f.ID
	}
	m["name"] = f.Name
	m["type"] = f.Type
	m["required"] = f.Required
	m["unique"] = f.Unique
	m["hidden"] = f.Hidden
	if f.System {
		m["system"] = true
	}
	if len(f.Default) > 0 {
		m["default"] = f.Default
	}
	return json.Marshal(m)
}

// apiCollection is a collection definition.
type apiCollection struct {
	ID         string                     `json:"id,omitempty"`
	Name       string                     `json:"name"`
	Type       string                     `json:"type,omitempty"`
	System     bool                       `json:"system,omitempty"`
	SchemaMode string                     `json:"schema_mode,omitempty"`
	Versioned  bool                       `json:"versioned"`
	Fields     []apiField                 `json:"fields"`
	ViewQuery  *string                    `json:"view_query,omitempty"`
	Options    map[string]json.RawMessage `json:"options,omitempty"`
	Version    int64                      `json:"version,omitempty"`
	Created    string                     `json:"created,omitempty"`
	Updated    string                     `json:"updated,omitempty"`
}

func (c apiCollection) tenantScoped() bool {
	return string(bytes.TrimSpace(c.Options["tenant_scoped"])) == "true"
}

// jsonEqual reports whether a and b hold the same JSON value.
func jsonEqual(a, b []byte) bool {
	var x, y any
	if json.Unmarshal(a, &x) != nil || json.Unmarshal(b, &y) != nil {
		return false
	}
	return reflect.DeepEqual(x, y)
}

// fieldTypes are the field types the server knows.
var fieldTypes = []string{"text", "number", "bool", "date", "email", "url", "select", "json", "array", "relation", "file", "password", "geo", "vector"}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
