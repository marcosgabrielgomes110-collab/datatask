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
- O servidor fala HTTP puro. Não exponha `bind_address` fora de loopback sem um
  proxy reverso com TLS.
- O painel guarda o token em `sessionStorage`; qualquer XSS teria acesso a ele.
  A aplicação aplica CSP e escapa conteúdo, mas trate o navegador como confiável.
- Jobs e logs podem conter segredos. Mantenha `jobs/` e `logs/` em `700`/`600`
  e não coloque credenciais em `action`.
- O rate limit é por IP. Com `trust_proxy_headers = true`, o servidor confia no
  primeiro IP de `X-Forwarded-For`; só habilite atrás de um proxy confiável,
  caso contrário o cliente pode falsificar o IP e escapar do limite.
- A auditoria em `audit.jsonl` registra IP, método, path, status, job e duração.
  Ela não grava token, corpo de request nem `action`, mas o path pode revelar
  nomes de jobs; proteja o arquivo como dado sensível.
- Se você encontrar token em log, histórico de shell, issue ou commit, rotacione
  imediatamente e remova o segredo do histórico.
