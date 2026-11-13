{{/* Names and labels. */}}
{{- define "forgebase.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "forgebase.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "forgebase.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "forgebase.selectorLabels" -}}
app.kubernetes.io/name: {{ include "forgebase.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "forgebase.labels" -}}
helm.sh/chart: {{ include "forgebase.chart" . }}
{{ include "forgebase.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "forgebase.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "forgebase.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* The pod name: the StatefulSet has exactly one replica. */}}
{{- define "forgebase.podName" -}}
{{- printf "%s-0" (include "forgebase.fullname" .) -}}
{{- end -}}

{{/*
Image reference. The "lite" variant is the SQLite-only build, published (when
you publish it) under the same repository with a "-lite" tag suffix.
*/}}
{{- define "forgebase.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- if and (eq .Values.image.variant "lite") (not (hasSuffix "-lite" $tag)) -}}
{{- $tag = printf "%s-lite" $tag -}}
{{- end -}}
{{- if .Values.image.digest -}}
{{- printf "%s:%s@%s" .Values.image.repository $tag .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}
{{- end -}}

{{/* "true" when ForgeBase runs the bundled PostgreSQL engine in the pod. */}}
{{- define "forgebase.managedPostgres" -}}
{{- if and (eq .Values.database.backend "postgres") (eq .Values.database.postgres.mode "managed") -}}true{{- end -}}
{{- end -}}

{{/* "true" when the database is an external PostgreSQL server. */}}
{{- define "forgebase.externalPostgres" -}}
{{- if and (eq .Values.database.backend "postgres") (eq .Values.database.postgres.mode "external") -}}true{{- end -}}
{{- end -}}

{{/* Secret and key that hold the external database URL. */}}
{{- define "forgebase.databaseSecretName" -}}
{{- if .Values.database.postgres.external.existingSecret -}}
{{- .Values.database.postgres.external.existingSecret -}}
{{- else -}}
{{- printf "%s-database" (include "forgebase.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "forgebase.databaseSecretKey" -}}
{{- if .Values.database.postgres.external.existingSecret -}}
{{- .Values.database.postgres.external.existingSecretKey -}}
{{- else -}}url{{- end -}}
{{- end -}}

{{/* Secret and key that hold the bootstrap administrator's password. */}}
{{- define "forgebase.adminSecretName" -}}
{{- if .Values.admin.bootstrap.existingSecret -}}
{{- .Values.admin.bootstrap.existingSecret -}}
{{- else -}}
{{- printf "%s-admin" (include "forgebase.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "forgebase.adminSecretKey" -}}
{{- if .Values.admin.bootstrap.existingSecret -}}
{{- .Values.admin.bootstrap.existingSecretKey -}}
{{- else -}}password{{- end -}}
{{- end -}}

{{/* Names of the PVCs. */}}
{{- define "forgebase.dataClaimName" -}}
{{- default (printf "%s-data" (include "forgebase.fullname" .)) .Values.persistence.existingClaim -}}
{{- end -}}

{{- define "forgebase.backupClaimName" -}}
{{- default (printf "%s-backups" (include "forgebase.fullname" .)) .Values.backup.destination.volume.existingClaim -}}
{{- end -}}

{{/* "true" when backups go to a separate volume mounted at /backups. */}}
{{- define "forgebase.backupVolume" -}}
{{- if and .Values.backup.enabled (eq .Values.backup.destination.type "volume") -}}true{{- end -}}
{{- end -}}

{{/*
Chart-level checks. Values the schema cannot cross-check fail here, at render
time, with a message that says what to change.
*/}}
{{- define "forgebase.validate" -}}
{{- if and (eq .Values.image.variant "lite") (eq .Values.database.backend "postgres") (eq .Values.database.postgres.mode "managed") -}}
{{- fail "image.variant=lite has no bundled PostgreSQL: set database.backend=sqlite, or database.postgres.mode=external" -}}
{{- end -}}
{{- if and (eq .Values.database.backend "sqlite") (ne .Values.database.postgres.mode "managed") -}}
{{- fail "database.postgres.mode only applies to database.backend=postgres" -}}
{{- end -}}
{{- if include "forgebase.externalPostgres" . -}}
{{- if and (not .Values.database.postgres.external.existingSecret) (not .Values.database.postgres.external.url) -}}
{{- fail "external PostgreSQL needs its URL: set database.postgres.external.existingSecret (a Secret with the URL under existingSecretKey), or database.postgres.external.url" -}}
{{- end -}}
{{- end -}}
{{- if .Values.admin.bootstrap.enabled -}}
{{- if not .Values.admin.bootstrap.email -}}
{{- fail "admin.bootstrap.enabled needs admin.bootstrap.email" -}}
{{- end -}}
{{- if and (not .Values.admin.bootstrap.existingSecret) (not .Values.admin.bootstrap.password) -}}
{{- fail "admin.bootstrap.enabled needs the password: set admin.bootstrap.existingSecret (key existingSecretKey), or admin.bootstrap.password" -}}
{{- end -}}
{{- end -}}
{{- if and .Values.persistence.enabled (gt (len .Values.persistence.accessModes) 1) -}}
{{- fail "persistence.accessModes: ForgeBase has one writer; give exactly one access mode (ReadWriteOnce or ReadWriteOncePod)" -}}
{{- end -}}
{{- if and .Values.backup.enabled (not .Values.persistence.enabled) -}}
{{- fail "backup.enabled needs persistence.enabled: a backup of an emptyDir is worthless" -}}
{{- end -}}
{{- if and .Values.serviceMonitor.enabled (not .Values.serviceMonitor.authorization.secretName) (not .Values.serviceMonitor.unauthenticated) -}}
{{- fail "serviceMonitor.enabled: /metrics is closed to Prometheus by default (server.ops_access=loopback). Set serviceMonitor.authorization.secretName (a Secret holding an admin service-account API key with the system:read scope), or serviceMonitor.unauthenticated=true together with opsAccess=public" -}}
{{- end -}}
{{- if and .Values.serviceMonitor.enabled .Values.serviceMonitor.unauthenticated (ne .Values.opsAccess "public") -}}
{{- fail "serviceMonitor.unauthenticated needs opsAccess=public; read docs/security.md before you do that" -}}
{{- end -}}
{{- end -}}
