# Política de segurança

## Versões suportadas

Apenas a versão mais recente publicada neste repositório recebe correções de
segurança.

## Como reportar

Não abra issue pública com detalhes de vulnerabilidade. Use a aba
[Security](https://github.com/marcosgabrielgomes110-collab/datatask/security)
→ **Advisories** → **Report a vulnerability** para enviar o relato de forma
privada. Se o botão não estiver disponível, abra um draft advisory na mesma aba
ou contate o mantenedor pelo perfil do GitHub. Inclua:

- versão/commit afetado;
- passos de reprodução;
- impacto estimado (ex.: leitura de token, execução de comando, DoS);
- sugestão de correção, se houver.

Você receberá uma confirmação de recebimento. Corrigimos e publicamos um
advisory com crédito, se desejado.

## Modelo de ameaça (resumo)

- **Token da API = execução de comandos.** Qualquer pessoa com o token pode
  criar jobs e rodar comandos como o usuário do serviço.
- O servidor usa HTTP por padrão. Bind fora de loopback é recusado no boot, a
  menos que `tls_terminated_by_proxy = true` + `trusted_proxy_ips` indiquem um
  proxy TLS, ou que `tls_cert_file`/`tls_key_file` habilitem TLS nativo
  (`rustls`). Em ambos os casos, restrinja o acesso e proteja a chave (`600`).
- Com `tls_terminated_by_proxy = true`, peers não-loopback fora da allowlist
  recebem `403`; os confiáveis precisam enviar `X-Forwarded-Proto: https`
  (exceto `/health` e `/readyz`). O header é falsificável, por isso a allowlist
  e o firewall são obrigatórios.
- O painel guarda o token em `sessionStorage`; qualquer XSS teria acesso a ele.
  A aplicação aplica CSP e escapa conteúdo, mas trate o navegador como confiável.
- Jobs e logs podem conter segredos. Mantenha `jobs/` e `logs/` em `700`/`600`
  e não coloque credenciais em `action`.
- O rate limit é por IP. Com `trust_proxy_headers = true`, o servidor só usa o
  primeiro IP de `X-Forwarded-For` quando a conexão vem de loopback ou de um IP
  em `trusted_proxy_ips`; caso contrário, usa o IP real do peer. Mantenha a
  allowlist e o firewall corretos para não confiar em headers de terceiros.
- A auditoria em `audit.jsonl` registra IP, método, path, status, job e duração.
  Ela não grava token, corpo de request nem `action`, mas o path pode revelar
  nomes de jobs; proteja o arquivo como dado sensível.
- `/metrics` é anônimo apenas para loopback. Scrapes remotos exigem Bearer e
  expõem somente agregados, sem nomes de jobs, comandos ou saídas.
- O histórico em `history.jsonl` guarda nomes de jobs, horários, resultados,
  exit codes e duração. Não inclui `action` nem saída de log, mas revele o
  arquivo apenas para o usuário do serviço (`600`).
- `notify_url` permite que quem possui o token faça o servidor enviar POSTs
  para destinos arbitrários (potencial SSRF). Redirecionamentos não são
  seguidos; mantenha o token restrito e use apenas destinos confiáveis.
- `backup_alert_url` tem o mesmo risco de SSRF do webhook de jobs; use somente
  destinos confiáveis e proteja o token.
- A exportação inclui `action` e `notify_url` dos jobs, que podem conter
  segredos. Trate o JSON exportado como credencial (`600`) e não o publique.
  A importação pode sobrescrever jobs existentes; revise a prévia antes.
- Backups automáticos também contêm `action`/`notify_url`; mantenha
  `backup_dir` em `700` e os arquivos em `600`, com retenção limitada. O
  sidecar `.sha256` detecta corrupção/tampering, mas não substitui permissões
  nem criptografia de disco.
- O envio externo usa credenciais do ambiente (`ssh`/`aws`). O arquivo de
  `backup_encryption_key_file` é uma senha GPG e deve ficar `600`; sem ele, o
  JSON sobe em claro para o destino remoto.
- Se você encontrar token em log, histórico de shell, issue ou commit, rotacione
  imediatamente e remova o segredo do histórico.
