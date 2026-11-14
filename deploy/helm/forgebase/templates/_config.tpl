{{/*
forgebase.toml
--------------
The settings the chart derives from first-class values, deep-merged with
.Values.config (which wins), rendered as TOML by a small renderer. Helm's own
toToml writes every whole number as a float ("port = 8090.0"), which the
binary rejects, so the renderer prints whole numbers as integers.
*/}}

{{- define "forgebase.configBase" -}}
data_dir: /data
server:
  host: 0.0.0.0
  port: {{ int .Values.containerPort }}
  ops_access: {{ .Values.opsAccess | quote }}
  {{- with .Values.publicUrl }}
  public_url: {{ . | quote }}
  {{- end }}
database:
  backend: {{ .Values.database.backend | quote }}
  {{- if eq .Values.database.backend "postgres" }}
  postgres:
    mode: {{ .Values.database.postgres.mode | quote }}
  {{- end }}
log:
  format: json
  level: {{ .Values.logLevel | quote }}
{{- if .Values.trustForwardedFor }}
auth:
  trust_forwarded_for: true
{{- end }}
{{- if .Values.resp.enabled }}
resp:
  enabled: true
  host: 0.0.0.0
  port: {{ int .Values.resp.port }}
{{- end }}
{{- if ne .Values.tls.mode "off" }}
tls:
  mode: {{ .Values.tls.mode | quote }}
  {{- if and (eq .Values.tls.mode "files") .Values.tls.existingSecret }}
  cert_file: /etc/forgebase-tls/tls.crt
  key_file: /etc/forgebase-tls/tls.key
  {{- end }}
{{- end }}
{{- end -}}

{{- define "forgebase.configDict" -}}
{{- $base := include "forgebase.configBase" . | fromYaml -}}
{{- if and (ne .Values.tls.mode "off") .Values.tls.settings -}}
{{- $_ := set $base "tls" (mergeOverwrite (get $base "tls") (deepCopy .Values.tls.settings)) -}}
{{- end -}}
{{- toJson (mergeOverwrite $base (deepCopy .Values.config)) -}}
{{- end -}}

{{- define "forgebase.tomlKey" -}}
{{- if regexMatch "^[A-Za-z0-9_-]+$" . -}}{{ . }}{{- else -}}{{ . | toJson }}{{- end -}}
{{- end -}}

{{- define "forgebase.tomlValue" -}}
{{- $k := kindOf . -}}
{{- if eq $k "string" -}}
{{- . | toJson -}}
{{- else if eq $k "bool" -}}
{{- . -}}
{{- else if has $k (list "int" "int64" "float64") -}}
{{- $f := float64 . -}}
{{- if eq $f (floor $f) -}}{{ int64 . }}{{- else -}}{{ . }}{{- end -}}
{{- else if eq $k "slice" -}}
[{{- range $i, $v := . -}}{{- if $i }}, {{ end -}}{{- include "forgebase.tomlValue" $v -}}{{- end -}}]
{{- else -}}
{{- fail (printf "config: cannot render a value of kind %q as TOML (use scalars, lists of scalars and tables)" $k) -}}
{{- end -}}
{{- end -}}

{{/* Arguments: dict "path" "a.b" "map" <map>. Scalars first, then sub-tables. */}}
{{- define "forgebase.tomlTable" -}}
{{- $path := .path -}}
{{- $m := .map -}}
{{- range $k := (keys $m | sortAlpha) -}}
{{- $v := get $m $k -}}
{{- if not (or (kindIs "map" $v) (kindIs "invalid" $v)) }}
{{ include "forgebase.tomlKey" $k }} = {{ include "forgebase.tomlValue" $v }}
{{- end -}}
{{- end -}}
{{- range $k := (keys $m | sortAlpha) -}}
{{- $v := get $m $k -}}
{{- if kindIs "map" $v -}}
{{- $sub := printf "%s%s" (ternary "" (printf "%s." $path) (eq $path "")) (include "forgebase.tomlKey" $k) }}

[{{ $sub }}]
{{- include "forgebase.tomlTable" (dict "path" $sub "map" $v) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "forgebase.toml" -}}
# Rendered by the ForgeBase Helm chart from values (config and the settings
# derived from them). Do not edit; change the release's values.
{{ include "forgebase.tomlTable" (dict "path" "" "map" (include "forgebase.configDict" . | fromJson)) | trim }}
{{ end -}}

{{/* Probe scheme: probes.scheme when set, else HTTPS with native TLS on and HTTP otherwise. */}}
{{- define "forgebase.probeScheme" -}}
{{- if .Values.probes.scheme -}}
{{- .Values.probes.scheme -}}
{{- else if ne .Values.tls.mode "off" -}}
HTTPS
{{- else -}}
HTTP
{{- end -}}
{{- end -}}
