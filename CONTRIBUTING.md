# Contribuindo com o DataTask

Obrigado pelo interesse. Este projeto é um agendador local que executa comandos
arbitrários, então mudanças em autenticação, permissões, execução de processos e
logs são tratadas como sensíveis.

## Ambiente

- Rust stable (edition 2024)
- Linux com `/bin/sh`
- `openssl` para gerar tokens
- `gh` (opcional, para o fluxo de PR)

```bash
./datatask.sh setup      # config + build
./datatask.sh run        # execução local
```

## Antes de abrir um PR

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
bash -n datatask.sh
```

Todos precisam passar. A CI repete exatamente esses passos.

## Regras do repositório

- Nunca faça commit de `config.toml`, `api_token`, `jobs/` ou `logs/`.
  Eles já estão no `.gitignore`; confira com `git status --ignored`.
- Não coloque segredos em `action` de jobs, testes ou exemplos.
- Preserve a compatibilidade com configs antigas: campos novos devem ter
  `#[serde(default)]` e valores padrão seguros.
- Mudanças de segurança devem vir com teste ou justificativa no PR.

## Commits

Mensagens em inglês, no imperativo, com escopo curto. Todo commit termina com um
trailer informando a próxima implementação planejada:

```text
feat(security): harden API token handling

Próxima implementação: rate limit por IP e auditoria JSONL das operações.
```

## Reportar vulnerabilidades

Não abra issue pública. Use o canal descrito em [SECURITY.md](SECURITY.md).
