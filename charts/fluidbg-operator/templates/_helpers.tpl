{{- define "fluidbg.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "fluidbg.fullname" -}}
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

{{- define "fluidbg.labels" -}}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "fluidbg.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "fluidbg.selectorLabels" -}}
app.kubernetes.io/name: {{ include "fluidbg.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: operator
{{- end -}}

{{- define "fluidbg.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "fluidbg.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "fluidbg.postgresSecretName" -}}
{{- default (printf "%s-postgres" (include "fluidbg.fullname" .)) .Values.stateStore.postgres.urlSecretName -}}
{{- end -}}

{{- define "fluidbg.cosmosConnectionStringSecretName" -}}
{{- default (printf "%s-cosmos-connection" (include "fluidbg.fullname" .)) .Values.stateStore.cosmos.connectionStringSecretName -}}
{{- end -}}

{{- define "fluidbg.cosmosAccountKeySecretName" -}}
{{- default (printf "%s-cosmos-account-key" (include "fluidbg.fullname" .)) .Values.stateStore.cosmos.accountKeySecretName -}}
{{- end -}}

{{- define "fluidbg.rabbitmqManagerSecretName" -}}
{{- default (printf "%s-rabbitmq-manager" (include "fluidbg.fullname" .)) .Values.builtinPlugins.rabbitmq.manager.amqpUrlSecretName -}}
{{- end -}}
