# DataTask

Agendador de tarefas em Rust com API REST local. Os jobs continuam armazenados
como arquivos JSON em `jobs/` e a saída de cada comando é acrescentada em
`logs/<nome>.log`.

## Configuração

Edite `config.toml`:

```toml
bind_address = "127.0.0.1"
port = 8080
# Necessário apenas se bind_address não for loopback e houver proxy TLS na frente.
tls_terminated_by_proxy = false
# IPs de proxies remotos autorizados a enviar X-Forwarded-*; loopback é sempre confiável.
trusted_proxy_ips = []
# TLS nativo opcional (não combine com tls_terminated_by_proxy).
# tls_cert_file = "/etc/datatask/cert.pem"
# tls_key_file = "/etc/datatask/key.pem"
# Recomendado: token em arquivo separado, com permissão 600.
api_token_file = "/etc/datatask/api_token"
# Alternativa (legado): token direto na config.
# api_token = "..."
jobs_dir = "jobs"
logs_dir = "logs"
poll_interval_seconds = 1
command_timeout_seconds = 300
notification_timeout_seconds = 10
max_concurrent_jobs = 4
log_max_bytes = 10485760
rate_limit_per_minute = 600
auth_failure_limit = 10
auth_failure_window_seconds = 60
auth_lockout_seconds = 300
trust_proxy_headers = false
audit_log_file = "logs/audit.jsonl"
history_file = "logs/history.jsonl"
```

Gere um token com `openssl rand -hex 32`. Use **apenas uma** das opções
`api_token_file` ou `api_token`. O arquivo do token precisa ter permissão
`600`; o servidor recusa iniciar se grupo/outros puderem lê-lo. Todas as rotas
`/api` exigem autenticação Bearer. Por padrão, o servidor fica restrito ao
próprio computador (`127.0.0.1`). Se `bind_address` não for loopback, o servidor
**recusa iniciar** a menos que `tls_terminated_by_proxy = true` confirme que há
um proxy TLS na frente; mesmo assim, restrinja a porta ao proxy no firewall. A
API não fala TLS nativamente — veja a seção abaixo.

`command_timeout_seconds` limita a duração de cada comando.
`notification_timeout_seconds` limita cada POST de webhook.
`max_concurrent_jobs` limita quantos comandos rodam ao mesmo tempo (os
agendamentos excedentes são tentados de novo no próximo ciclo).
`log_max_bytes` rotaciona `logs/<nome>.log` para `.log.1` ao atingir o limite
(`0` desativa a rotação).

`rate_limit_per_minute` limita requisições por IP em `/api` (`0` desativa) e
`auth_failure_limit` bloqueia temporariamente um IP após N tokens inválidos,
durante `auth_lockout_seconds` (`0` desativa o bloqueio). Requests bloqueadas
respondem `429` com `Retry-After`. `trust_proxy_headers = true` faz o servidor
usar o primeiro IP de `X-Forwarded-For`; habilite **somente** atrás de um proxy
confiável, senão o cliente pode falsificar o IP e escapar do limite.

`audit_log_file` recebe uma linha JSON por evento (`api.request`, `job.run`,
`auth.failure`, `auth.lockout`, `request.rejected`) com data, IP, método, path,
status, job e duração. Corpo de request, token e `action` **não** são gravados.
O arquivo é `600` e rotaciona para `.1` em 5 MiB. Use `""` para desativar.

`history_file` guarda cada tentativa de execução em JSONL (`timestamp`, job,
origem, tentativa, resultado, exit code, duração e timeout). O arquivo é `600`,
rotaciona para `.1` em 20 MiB e pode ser desativado com `""`; a API e o painel
continuam funcionando com histórico vazio.

O servidor aplica `umask 077` e mantém `jobs/` e `logs/` em `700`, com arquivos
`600`, porque ações e saídas de comandos podem conter segredos. A config também
deve ser `600`; o `install` do script já faz isso e move o token do serviço para
`/etc/datatask/api_token`.

## TLS e exposição na rede

O DataTask pode terminar TLS **nativamente** com `rustls` ou ficar atrás de um
proxy reverso. Para o modo proxy, o recomendado é manter
`bind_address = "127.0.0.1"` e colocar o proxy no mesmo host:

```caddyfile
tasks.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

O Caddy define `X-Forwarded-Proto`/`X-Forwarded-For` automaticamente. Para nginx:

```nginx
server {
    listen 443 ssl;
    server_name tasks.example.com;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Se o proxy estiver em **outro host**, o bind precisa ser acessível a ele e o IP
do proxy precisa estar na allowlist:

```toml
bind_address = "0.0.0.0"
tls_terminated_by_proxy = true
trusted_proxy_ips = ["10.0.0.10"]
trust_proxy_headers = true
```

Nesse caso:

- o firewall deve permitir a porta **somente** para os IPs de `trusted_proxy_ips`;
- `tls_terminated_by_proxy = true` é uma afirmação explícita, não detecta TLS;
- peers não-loopback fora da allowlist recebem `403`;
- peers confiáveis sem `X-Forwarded-Proto: https` recebem `426`
  (exceto `/health` e `/readyz`);
- `X-Forwarded-For` só é aceito quando o peer é loopback ou está em
  `trusted_proxy_ips`; caso contrário o IP real da conexão é usado no rate limit;
- o servidor envia `Strict-Transport-Security` quando o modo proxy TLS está ativo;
- loopback é sempre confiável, então um proxy no mesmo host não precisa de
  allowlist (e o bind deve continuar em `127.0.0.1`).

### TLS nativo

Para terminar TLS no próprio processo, configure os dois arquivos e **não**
use `tls_terminated_by_proxy`:

```toml
bind_address = "0.0.0.0"
tls_cert_file = "/etc/datatask/cert.pem"
tls_key_file = "/etc/datatask/key.pem"
```

A chave precisa ser `600`; o servidor recusa iniciar se grupo/outros puderem
lê-la. O bind fora de loopback é permitido quando o TLS nativo está ativo.

```bash
sudo openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout /etc/datatask/key.pem -out /etc/datatask/cert.pem \
  -subj "/CN=tasks.local"
sudo chmod 600 /etc/datatask/key.pem
```

Com `certbot`, aponte `tls_cert_file`/`tls_key_file` para
`/etc/letsencrypt/live/<domínio>/fullchain.pem` e `privkey.pem` (o usuário do
serviço precisa de acesso de leitura).

Recarregar sem reiniciar:

```bash
sudo ./datatask.sh reload     # systemctl reload datatask (SIGHUP)
# ou: sudo kill -HUP $(systemctl show -p MainPID --value datatask)
```

O certificado antigo continua ativo se a recarga falhar; o erro vai para o
journal. HSTS é enviado nos dois modos (nativo e proxy).

## Executar

```bash
cargo run --release
```

Abra `http://127.0.0.1:8080` no navegador para usar o painel. Informe o
`api_token` configurado no `config.toml` para conectar; o token fica guardado
somente na sessão atual do navegador.

> Importante: use o painel pelo endereço do servidor. Abrir
> `static/index.html` direto no navegador exibe a interface, mas não acessa a
> API. O CSS e o JS são servidos em `/app.css` e `/app.js` (também em
> `/assets/...`) e ficam embutidos no executável.

O painel tem tema escuro, layout responsivo (cartões no celular) e animações
que podem ser desativadas nas preferências. A navegação lateral separa:

- **Tarefas** — resumo (total, ativas, recorrentes, executadas hoje), busca,
  filtro por status, ordenação por coluna, paginação server-side, execução
  manual com indicador de “executando”, histórico do último resultado (sucesso,
  falha ou timeout), duplicação, edição, exportação e importação de jobs em JSON.
- **Logs** — lista de tarefas, visualizador com acompanhamento ao vivo
  (polling incremental por offset), filtro de linhas, quebra de linha,
  rolagem automática, download e limpeza do log.
- **Histórico** — tentativas persistidas com filtro por tarefa/resultado,
  paginação, tentativa, duração, exit code e origem (agenda ou manual).
- **Configurações** — conexão (conectar/desconectar), preferências (intervalo
  de atualização, intervalo do log ao vivo, animações, linhas compactas,
  quebra de linha) e informações do servidor.

Atalhos: `n` cria tarefa, `/` foca a busca e `r` atualiza os dados.

No formulário, escolha uma data única, vários horários por dia, intervalo
(inclusive restrito a um período do dia), dias da semana (um ou vários,
atalhos para dias úteis e fins de semana), dia do mês ou horário diário. Um
preview mostra a expressão gerada e a próxima execução estimada. As opções
avançadas da tarefa permitem definir um diretório de trabalho (o comando roda
nesse diretório) e um tempo limite individual em segundos. A opção cron
avançada continua disponível para agendamentos personalizados.

É possível passar o caminho de outro arquivo TOML como primeiro argumento:

```bash
cargo run --release -- /caminho/para/config.toml
```

## Script de gerenciamento

O `datatask.sh` automatiza setup, execução local e instalação como serviço. O
backend e o frontend (painel) rodam no **mesmo processo**: o binário embute
`index.html`, `app.css` e `app.js` e os serve na mesma porta da API.

```bash
./datatask.sh setup                 # assistente: endereço, porta, pastas, intervalos e senha
./datatask.sh run                   # backend + frontend (release), primeiro plano
./datatask.sh dev                   # backend + frontend via cargo run
./datatask.sh url                   # URL do painel
./datatask.sh token                 # mostra o token configurado

sudo ./datatask.sh install          # instala e ativa o serviço systemd
sudo ./datatask.sh status           # estado + health check + checagem do frontend
sudo ./datatask.sh logs             # journalctl -u datatask -f
sudo ./datatask.sh restart
sudo ./datatask.sh reload           # recarrega o certificado TLS (SIGHUP)
sudo ./datatask.sh uninstall        # remove o serviço, mantém config e dados
sudo ./datatask.sh uninstall --purge
```

O `setup` é interativo: cada campo mostra o valor atual/padrão entre colchetes e
**Enter aceita o padrão**.

```
Endereço de escuta [127.0.0.1]:
Porta do painel [8080]:
Diretório de tarefas [jobs]:
Diretório de logs [logs]:
Verificação de agenda (segundos) [1]:
Timeout de comando (segundos) [300]:
Senha da API (mín. 32 caracteres, Enter gera automaticamente):
Confirme a senha:
```

A senha é digitada sem eco, confirmada e precisa ter 32+ caracteres;
**Enter** mantém a senha existente ou gera uma automaticamente. Para automação,
use `DATATASK_TOKEN` (32+ caracteres) ou `--yes` para aceitar tudo sem
perguntas. Também é possível pré-responder com `--bind`, `--port`,
`--jobs-dir`, `--logs-dir`, `--poll` e `--timeout`.

Opções: `--port`, `--bind`, `--jobs-dir`, `--logs-dir`, `--poll`, `--timeout`,
`--config`, `--user`, `--rebuild`, `--no-build`, `--with-deps`, `--dry-run`
(mostra o unit file sem instalar), `--keep-config` (não sobrescreve a config do
serviço) e `-y`.

O `install` sincroniza a config do projeto com `/etc/datatask/config.toml` a
cada execução (com backup `.bak`), mantendo **uma única senha/porta** entre
`setup`, execução local e serviço. `token` e `url`, quando o serviço está
instalado, mostram a config do serviço; use `--local` ou `--service` para
forçar. O comando `doctor` compara as duas configs, testa health e valida o
token contra a API.

```bash
./datatask.sh doctor                # diagnóstico de token/config/serviço
sudo ./datatask.sh token --service  # token que o painel deve usar
```

No `install`, o serviço é criado com:

- binário em `/usr/local/bin/datatask` (com o painel embutido);
- config em `/etc/datatask/config.toml` (copia a config do projeto criada no
  `setup` — mesma senha/porta — ou gera um token forte se não existir;
  permissões `600`);
- token do serviço em `/etc/datatask/api_token` (`600`, referenciado por
  `api_token_file` na config);
- tarefas e logs em `/var/lib/datatask/{jobs,logs}` (`700`, arquivos `600`);
- usuário do serviço = usuário que chamou o `sudo` (ou `--user`); se não for
  possível, cria o usuário de sistema `datatask`;
- `Restart=on-failure`, `ExecReload` via `SIGHUP` e endurecimento do systemd
  (`NoNewPrivileges`, `ProtectSystem`, `CapabilityBoundingSet=`, `UMask=0077`,
  `LimitCORE=0`, entre outros).

Após ativar, o script valida `/health`, `/readyz`, `/`, `/app.css` e `/app.js`,
garantindo que backend **e** frontend estão no ar. Reexecutar `install` preserva
a config e os dados existentes.

> Atualizando uma instalação anterior: rode `sudo ./datatask.sh install` uma vez.
> Ele move o token para `/etc/datatask/api_token`, aplica as permissões `600`/`700`
> e reinstala o binário. Confirme depois com `sudo ./datatask.sh doctor`.

## Segurança e limites

- O token é comparado pelo SHA-256 em tempo constante e nunca é impresso em
  mensagens de erro de configuração (o erro mostra apenas linha/coluna). Tokens
  de exemplo/placeholder são rejeitados no boot.
- O painel recebe `Content-Security-Policy`, `X-Content-Type-Options`,
  `X-Frame-Options`, `Referrer-Policy` e `Cache-Control: no-store` nas rotas
  `/api`.
- Rate limit por IP e bloqueio temporário após falhas de autenticação; eventos
  relevantes vão para o audit JSONL, sem token, corpo ou `action`.
- Fora de loopback, o boot exige `tls_terminated_by_proxy = true` e
  `trusted_proxy_ips`, ou TLS nativo com `tls_cert_file`/`tls_key_file`; requests
  de proxies não listados recebem `403`, e sem `X-Forwarded-Proto: https`
  recebem `426` e HSTS no modo proxy.
- No TLS nativo a chave precisa ser `600`, o certificado é recarregado por
  SIGHUP/`systemctl reload` e HSTS é enviado.
- `/readyz` informa se o scheduler está ativo (lag do último tick) e quantos
  jobs estão em execução; `/health` continua sendo apenas liveness.
- `/metrics` só é anônimo para loopback; acesso remoto exige Bearer e expõe
  apenas agregados (sem nomes de jobs, comandos ou saídas).
- `notify_url` faz POST para um destino escolhido por quem tem o token. Isso
  pode alcançar serviços internos (SSRF); redirecionamentos são desligados, mas
  trate o token como credencial administrativa e restrinja destinos confiáveis.
- Cada comando roda em seu próprio grupo de processos; ao estourar o timeout,
  o grupo inteiro é encerrado (não apenas o `/bin/sh`).
- O processo adquire um `flock` exclusivo em `jobs/.datatask.lock`; uma segunda
  instância com o mesmo `jobs_dir` recusa iniciar, evitando execução duplicada.
  O lock depende de `flock` do sistema de arquivos (evite NFS sem suporte).
- O desligamento (SIGTERM/Ctrl+C) para o agendador e aguarda até 10 s pelos
  jobs em execução, para que os logs recebam o rodapé.
- Ações de jobs executam comandos arbitrários com o usuário do serviço. Não
  coloque segredos dentro de `action`: eles ficariam no JSON, na API e na linha
  de comando visível em `ps`. Prefira ler segredos de um arquivo `600` ou de um
  gerenciador de segredos dentro do próprio comando.
- Se o token vazar, o atacante executa comandos como o usuário do serviço.
  Rode o serviço com usuário dedicado (`--user datatask`) e mantenha
  `bind_address = "127.0.0.1"`.

## Agendamentos

Use cron de cinco campos (`minuto hora dia mês dia-da-semana`), por exemplo:

- `* * * * *` — todo minuto
- `0 * * * *` — toda hora, no minuto zero
- `30 8 * * *` — todos os dias às 08:30
- `*/15 * * * *` — a cada 15 minutos
- `0 9 * * 1-5` — às 09:00 de segunda a sexta

Também são aceitos horários únicos nos formatos `DD MM AAAA HH MM` e
`AAAA-MM-DD HH:MM`. O status do job é `ativo` ou `desativo`. Jobs únicos
ficam desativados depois da tentativa de execução; jobs cron permanecem ativos.

Campos opcionais por job:

- `working_dir` define o diretório em que o comando é executado (precisa existir
  no servidor; vazio usa o diretório do servidor);
- `timeout_seconds` limita a duração do comando somente para esse job
  (`0` usa o padrão `command_timeout_seconds` de `config.toml`);
- `retries` é o número de tentativas extras após uma falha ou timeout
  (`0` a `10`, padrão `0`);
- `retry_delay_seconds` é o atraso inicial do backoff exponencial
  (`1` a `3600`, padrão `5`): 5 s, 10 s, 20 s… até o teto de 1 h;
- `notify_url` é um webhook `http`/`https` chamado com `POST` **somente após
  esgotar as tentativas** e o job continuar em falha/timeout.

Para limpar `working_dir` em um `PATCH`, envie `""`; para limpar o webhook,
envie `""`; para desativar retries, envie `retries: 0`. O backoff é mantido em
memória: se o processo reiniciar durante a espera, as tentativas pendentes são
perdidas (o próximo agendamento cron tenta novamente).

O payload do webhook não inclui `action` nem saída de log:

```json
{
  "event": "job.failed",
  "job": "backup",
  "description": "Backup diário",
  "result": "error",
  "exit_code": 7,
  "timed_out": false,
  "duration_ms": 1234,
  "attempts": 3,
  "max_attempts": 3,
  "timestamp": "2026-11-01T07:00:00-03:00"
}
```

Redirecionamentos HTTP não são seguidos e o timeout é
`notification_timeout_seconds`.

## API

Todas as rotas abaixo exigem autenticação Bearer, exceto `/health`, `/readyz`
e `/metrics` via loopback (acesso remoto a `/metrics` continua exigindo Bearer).

- `GET /health` — liveness do processo
- `GET /readyz` — prontidão: scheduler ativo, lag do último tick e jobs em execução
- `GET /metrics` — métricas Prometheus; loopback não exige token, acesso remoto exige Bearer
- `GET /api/info` — versão, diretórios, intervalos, contagens e jobs em execução
- `GET /api/history?job=&result=&limit=&offset=` — tentativas persistidas, mais
  recentes primeiro; `result` aceita `success`, `error` ou `timeout`; `limit`
  padrão 50 (máx. 200)
- `GET /api/jobs` — lista completa (array); com `?limit=&offset=&q=&status=&sort=&order=`
  retorna `{items,total,limit,offset}` (`limit` padrão 50, máximo 500).
  `sort` aceita `name`, `schedule`, `status` ou `last`; `order` é `asc`/`desc`.
  A listagem usa cache em memória invalidado por mtime/tamanho dos arquivos
- `GET /api/jobs/export` — baixa um documento JSON com `format`, `version`,
  `exported_at` e `jobs` (campos configuráveis, sem histórico de execução)
- `POST /api/jobs/import` — importa `{format,version,jobs,on_conflict,dry_run}`;
  `on_conflict` aceita `skip` (padrão) ou `overwrite`, e `dry_run: true`
  retorna a prévia sem gravar
- `POST /api/jobs` — criar job; JSON: `name`, `description`, `datetime`,
  `action` e, opcionalmente, `status`, `working_dir`, `timeout_seconds`,
  `retries`, `retry_delay_seconds` e `notify_url`
- `GET /api/jobs/{name}` — consultar job (inclui `version`, `last_result`,
  `last_exit_code` e `last_duration_ms` após a primeira execução); a resposta
  traz `ETag: "<version>"`
- `PATCH /api/jobs/{name}` — alterar `description`, `datetime`, `action`,
  `status`, `working_dir`, `timeout_seconds`, `retries`,
  `retry_delay_seconds` ou `notify_url`. Envie `If-Match: "<version>"` para
  concorrência otimista; versão divergente retorna `409` com o job atual. Sem
  `If-Match`, o comportamento legado (last-write-wins) é mantido.
- `POST /api/jobs/{name}/activate` — ativar
- `POST /api/jobs/{name}/deactivate` — desativar
- `POST /api/jobs/{name}/run` — dispara a execução e responde `202` na hora;
  acompanhe pela lista (`running` em `/api/info`) e pelo log
- `GET /api/jobs/{name}/logs` — ler até os últimos 64 KiB do log; use
  `?download=1` para baixar como anexo
- `GET /api/jobs/{name}/logs/tail?offset=N` — leitura incremental em JSON
  (`content`, `offset`, `size`, `exists`) usada pelo log ao vivo
- `DELETE /api/jobs/{name}/logs` — limpar o log da tarefa
- `DELETE /api/jobs/{name}` — excluir job (não exclui o log)

O log de cada execução recebe um cabeçalho com data/hora e um rodapé com
duração, código de saída e resultado.

Exemplo de criação, supondo que `DATATASK_API_TOKEN` contenha o token:

```bash
curl -X POST http://127.0.0.1:8080/api/jobs \
  -H "Authorization: Bearer ${DATATASK_API_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"name":"exemplo","description":"Tarefa de teste","datetime":"*/5 * * * *","action":"date","status":"ativo"}'
```

Os campos `action` executam comandos pelo shell do sistema. Crie ou altere jobs
somente por meio de clientes confiáveis e mantenha o token secreto.

## Concorrência otimista

Jobs têm `version`, incrementada em alterações de configuração (`PATCH`,
ativar/desativar e desativação automática de job único). O `GET` do job devolve
`ETag: "<version>"`. Para editar com segurança:

```bash
curl -X PATCH http://127.0.0.1:8080/api/jobs/exemplo \
  -H "Authorization: Bearer ${DATATASK_API_TOKEN}" \
  -H 'Content-Type: application/json' \
  -H 'If-Match: "3"' \
  -d '{"description":"nova descrição"}'
```

Se outro cliente alterou o job antes, a resposta é `409` com o job atual no
campo `current`; recarregue e refaça a edição. O painel usa esse fluxo
automaticamente.

## Exportar e importar jobs

A exportação baixa os campos configuráveis de todos os jobs (sem histórico de
execução):

```bash
curl -H "Authorization: Bearer ${DATATASK_API_TOKEN}" \
  -o jobs.json http://127.0.0.1:8080/api/jobs/export
```

O arquivo tem o formato:

```json
{
  "format": "datatask.jobs",
  "version": 1,
  "exported_at": "2026-11-04T10:00:00-03:00",
  "jobs": [
    {
      "name": "backup",
      "description": "Backup diário",
      "datetime": "0 3 * * *",
      "action": "tar -czf /backup/db.tgz /dados",
      "status": "ativo",
      "retries": 2,
      "retry_delay_seconds": 5,
      "notify_url": "https://exemplo.com/hook"
    }
  ]
}
```

Prévia sem gravar (`dry_run`) e importação real:

```bash
curl -X POST http://127.0.0.1:8080/api/jobs/import \
  -H "Authorization: Bearer ${DATATASK_API_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"format":"datatask.jobs","version":1,"jobs":[...],"on_conflict":"skip","dry_run":true}'
```

`on_conflict`:

- `skip` (padrão): jobs existentes ficam intactos e aparecem em `skipped`;
- `overwrite`: configuração é substituída, preservando `last_run`/resultado e
  incrementando `version`.

A resposta contém `created`, `overwritten`, `skipped`, `total`, `mode` e
`dry_run`. Jobs inválidos retornam `400` com a lista `errors` e nada é gravado.
O painel usa prévia e oferece as opções **Sobrescrever** ou **Ignorar**.

## Histórico de execuções

Cada tentativa (incluindo retries) é acrescentada em `history_file` como uma
linha JSON. O histórico sobrevive a reinícios, é exibido na aba **Histórico** e
pode ser consultado pela API:

```bash
curl -H "Authorization: Bearer $DATATASK_API_TOKEN" \
  "http://127.0.0.1:8080/api/history?job=backup&result=error&limit=50&offset=0"
```

O endpoint retorna `items`, `total`, `limit`, `offset` e `enabled`, com os
registros mais recentes primeiro. O arquivo rotaciona para `.1` em 20 MiB e
`history_file = ""` desativa a persistência.

## Métricas

`GET /metrics` expõe o formato de texto do Prometheus:

```bash
curl -s http://127.0.0.1:8080/metrics
```

```yaml
scrape_configs:
  - job_name: datatask
    static_configs:
      - targets: ["127.0.0.1:8080"]
```

Acesso:

- conexões de loopback não exigem token e não entram no rate limit;
- conexões remotas exigem `Authorization: Bearer <token>` e usam o mesmo rate
  limit/bloqueio por IP de `/api`;
- o endpoint continua sujeito ao HTTPS guard quando
  `tls_terminated_by_proxy` está ativo.

Métricas expostas:

- `datatask_up`, `datatask_build_info{version}`, `datatask_uptime_seconds`
- `datatask_jobs_total`, `datatask_jobs_active`, `datatask_jobs_running`
- `datatask_scheduler_ready`, `datatask_scheduler_lag_seconds`
- `datatask_job_runs_total{result="success|error|timeout"}`
- `datatask_job_duration_seconds` (histograma com buckets de 0,1 s a 300 s)

As métricas são agregadas: nomes de jobs, comandos e saídas não aparecem nelas.

## Desenvolvimento

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
bash -n datatask.sh
```

A CI em `.github/workflows/ci.yml` executa os mesmos passos. Leia
[CONTRIBUTING.md](CONTRIBUTING.md) antes de abrir um PR e
[SECURITY.md](SECURITY.md) para reportar vulnerabilidades.

## Licença

MIT — veja [LICENSE](LICENSE).
