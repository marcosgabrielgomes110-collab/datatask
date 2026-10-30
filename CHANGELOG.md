# Changelog

Todas as mudanças relevantes deste projeto são documentadas aqui.
O formato segue [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o
versionamento segue [SemVer](https://semver.org/lang/pt-BR/).

## [Unreleased]

### Adicionado

- Suporte a `api_token_file` com verificação de permissões `600`.
- Limite de jobs simultâneos (`max_concurrent_jobs`) e rotação de logs
  (`log_max_bytes`).
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
