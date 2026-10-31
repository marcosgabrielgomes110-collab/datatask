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
# Recomendado: token em arquivo separado, com permissão 600.
api_token_file = "/etc/datatask/api_token"
# Alternativa (legado): token direto na config.
# api_token = "..."
jobs_dir = "jobs"
logs_dir = "logs"
poll_interval_seconds = 1
command_timeout_seconds = 300
max_concurrent_jobs = 4
log_max_bytes = 10485760
rate_limit_per_minute = 600
auth_failure_limit = 10
auth_failure_window_seconds = 60
auth_lockout_seconds = 300
trust_proxy_headers = false
audit_log_file = "logs/audit.jsonl"
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

O servidor aplica `umask 077` e mantém `jobs/` e `logs/` em `700`, com arquivos
`600`, porque ações e saídas de comandos podem conter segredos. A config também
deve ser `600`; o `install` do script já faz isso e move o token do serviço para
`/etc/datatask/api_token`.

## Exposição na rede e TLS

O DataTask **não termina TLS**. O modo recomendado é manter
`bind_address = "127.0.0.1"` e colocar um proxy no mesmo host:

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
  filtro por status, ordenação por coluna, execução manual com indicador de
  “executando”, histórico do último resultado (sucesso, falha ou timeout),
  duplicação e edição.
- **Logs** — lista de tarefas, visualizador com acompanhamento ao vivo
  (polling incremental por offset), filtro de linhas, quebra de linha,
  rolagem automática, download e limpeza do log.
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
- `Restart=on-failure` e endurecimento do systemd (`NoNewPrivileges`,
  `ProtectSystem`, `CapabilityBoundingSet=`, `UMask=0077`, `LimitCORE=0`,
  entre outros).

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
  `trusted_proxy_ips`; requests de proxies não listados recebem `403`, e sem
  `X-Forwarded-Proto: https` recebem `426` e HSTS no modo proxy.
- `/readyz` informa se o scheduler está ativo (lag do último tick) e quantos
  jobs estão em execução; `/health` continua sendo apenas liveness.
- `/metrics` só é anônimo para loopback; acesso remoto exige Bearer e expõe
  apenas agregados (sem nomes de jobs, comandos ou saídas).
- Cada comando roda em seu próprio grupo de processos; ao estourar o timeout,
  o grupo inteiro é encerrado (não apenas o `/bin/sh`).
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

Campos opcionais por job: `working_dir` define o diretório em que o comando
é executado (precisa existir no servidor; vazio usa o diretório do servidor) e
`timeout_seconds` limita a duração do comando somente para esse job
(`0` usa o padrão `command_timeout_seconds` de `config.toml`). Para limpar
`working_dir` em um `PATCH`, envie `""`; para restaurar o tempo limite padrão,
envie `0`.

## API

Todas as rotas abaixo, exceto `/health` e `/readyz`, precisam do cabeçalho de
autenticação Bearer, com o valor configurado em `api_token`.

- `GET /health` — liveness do processo
- `GET /readyz` — prontidão: scheduler ativo, lag do último tick e jobs em execução
- `GET /metrics` — métricas Prometheus; loopback não exige token, acesso remoto exige Bearer
- `GET /api/info` — versão, diretórios, intervalos, contagens e jobs em execução
- `GET /api/jobs` — listar jobs
- `POST /api/jobs` — criar job; JSON: `name`, `description`, `datetime`,
  `action` e, opcionalmente, `status`, `working_dir` e `timeout_seconds`
- `GET /api/jobs/{name}` — consultar job (inclui `last_result`,
  `last_exit_code` e `last_duration_ms` após a primeira execução)
- `PATCH /api/jobs/{name}` — alterar `description`, `datetime`, `action`,
  `status`, `working_dir` ou `timeout_seconds`
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
