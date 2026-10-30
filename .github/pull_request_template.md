## Resumo

<!-- O que muda e por quê. -->

## Tipo de mudança

- [ ] Correção de bug
- [ ] Nova funcionalidade
- [ ] Segurança
- [ ] Documentação / CI
- [ ] Refatoração sem mudança de comportamento

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all-targets`
- [ ] `bash -n datatask.sh`
- [ ] Não incluí `config.toml`, `api_token`, `jobs/` ou `logs/`
- [ ] Mantive compatibilidade com configs antigas (campos novos com default)

## Impacto de segurança

<!-- Token, permissões, execução de processos, exposição de rede, logs. -->

## Próxima implementação

<!-- Obrigatório: o que fica planejado depois deste PR. -->
