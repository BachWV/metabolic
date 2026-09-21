# metabolic

Rust + SQLite 评论系统，内置管理页与独立前端组件。

1. 将 `examples/config.example.yaml` 复制到 `private/config.yaml`。
2. 运行 `cargo run -- hash-password`，将生成的哈希填入配置，并设置站点信息。
3. 运行 `cargo run --release -- serve --config private/config.yaml`。

管理入口：`/admin`。嵌入示例：`examples/embed.html`；Hugo 接入：`examples/hugo/comments.html`。
