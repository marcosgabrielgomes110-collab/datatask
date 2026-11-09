# Changelog

Todas as mudanças relevantes deste projeto são documentadas aqui.
O formato segue [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o
versionamento segue [SemVer](https://semver.org/lang/pt-BR/).

## [Unreleased]

### Adicionado

- Suporte a `api_token_file` com verificação de permissões `600`.
- Limite de jobs simultâneos (`max_concurrent_jobs`) e rotação de logs
  (`log_max_bytes`).
- Rate limit por IP (`rate_limit_per_minute`) e bloqueio temporário após falhas
  de autenticação (`auth_failure_limit`/`auth_lockout_seconds`).
- Auditoria JSONL de operações, execuções e falhas de autenticação, com rotação
  em 5 MiB e sem token/corpo/`action`.
- Endpoint `/readyz` com heartbeat do scheduler e jobs em execução.
- Endpoint `/metrics` no formato Prometheus com execuções por resultado,
  histograma de duração, jobs e saúde do scheduler; loopback sem token e
  acesso remoto autenticado.
- Retry por job com backoff exponencial (`retries`, `retry_delay_seconds`) e
  webhook `notify_url` em falha/timeout, sem enviar `action` ou log.
- Histórico persistente de tentativas em `history_file` (JSONL), com filtros,
  paginação e tela própria no painel.
- Concorrência otimista com `version`/`ETag`/`If-Match` no `PATCH` de jobs,
  retornando `409` e o job atual em caso de conflito.
- Lock de instância única via `flock` em `jobs/.datatask.lock`, impedindo dois
  agendadores de executarem os mesmos jobs.
- TLS nativo com `rustls` (`tls_cert_file`/`tls_key_file`), recarga por
  `SIGHUP`/`systemctl reload` e HSTS, mantendo o modo de proxy TLS.
- Cache em memória da listagem de jobs com invalidação por mtime/tamanho e
  paginação/filtros server-side em `/api/jobs` (`limit`, `offset`, `q`, `status`,
  `sort`, `order`), usados também pelo painel.
- Exportação e importação de jobs em JSON (`/api/jobs/export`,
  `/api/jobs/import`) com validação, prévia `dry_run` e modos `skip`/`overwrite`.
- Backup automático do diretório de jobs em JSON, com escrita atômica,
  validação, intervalo configurável e retenção (`backup_dir`,
  `backup_interval_seconds`, `backup_retention`).
- Sidecar SHA-256 dos backups, listagem/verificação via `/api/backups` e
  restauração guiada no painel com bloqueio de arquivos corrompidos.
- Envio opcional de backups para SSH (`scp`/`ssh`) ou S3 (`aws s3 cp`), com
  criptografia GPG simétrica opcional e verificação SHA-256 remota.
- Retenção remota: após cada upload, artefatos antigos e sidecars além de
  `backup_retention` são removidos do destino, com auditoria de falhas.
- Verificação periódica de integridade dos backups remotos
  (`backup_verify_interval_seconds`) e endpoint manual
  `POST /api/backups/verify`.
- Alertas por webhook (`backup_alert_url`) em falha de verificação ou ausência
  de backups remotos, com deduplicação e auditoria.
- Histórico persistente das verificações em `logs/backup-verify.jsonl`, com API
  (`/api/backups/verifications`) e tabela no painel.
- Resumo consolidado de saúde dos backups (`/api/backups/health` e indicador no
  painel) com upload, retenção, verificação, alertas e problemas.
- Exportação do relatório de saúde em JSON/CSV
  (`/api/backups/health/export`) com histórico de verificações e problemas.
- Envio periódico do relatório de saúde por webhook
  (`backup_health_report_url`), com disparo manual e auditoria de entrega.
- Envio direto por SMTP (`backup_health_report_email`, `smtp_*`) com TLS,
  credenciais opcionais e múltiplos destinatários.
- E-mails de saúde com corpo resumido e anexos JSON/CSV, assunto configurável
  por `smtp_subject` e remetente por `smtp_from`.
- Perfis de notificação por destino (`[[notification_profiles]]`) para webhook
  ou e-mail, com filtro de severidade (`ok`/`warning`/`error`), nome único,
  `enabled` e auditoria por perfil (`backup.report_profile`).
- Exigência de `tls_terminated_by_proxy` para bind fora de loopback, rejeição
  `426` sem `X-Forwarded-Proto: https` e HSTS quando atrás de proxy TLS.
- Allowlist `trusted_proxy_ips` para aceitar `X-Forwarded-For`/`Proto` apenas de
  proxies confiáveis; peers não listados recebem `403` no modo TLS proxy.
- Cabeçalhos de segurança e `Cache-Control: no-store` nas rotas da API.
- Shutdown gracioso com encerramento do grupo de processos dos jobs.
- CI, templates de issue/PR, política de segurança e guia de contribuição.

### Alterado

- Tokens de exemplo/placeholder são rejeitados no boot.
- Erros de TOML não imprimem mais a linha que pode conter o token.
- Comparação de token por SHA-256 em tempo constante.
- `jobs/` e `logs/` passam a ser criados com `700` e arquivos `600`.

### Corrigido

- Processos filhos/pipelines não ficam órfãos após timeout.
- Um job JSON corrompido não derruba mais a listagem da API.
- Escrita de jobs agora usa `fsync` e rename atômico.

## [0.1.0] - 2026-10-29

### Adicionado

- Agendador com cron e data/hora única.
- Painel web com tarefas, logs ao vivo e configurações.
- API REST com autenticação Bearer.
- Script de setup, execução e instalação via systemd.
