# 社区扩展目录

社区的 Wasm 扩展用一条命令安装:

```sh
lueying ext add github:作者/仓库#版本
```

版本是 tag 或提交号,必填——装的就是钉住的那一版,同一版本重复装,内容一样。取货顺序:

1. 该版本的 GitHub Release 里附带的 `.luext` 包(推荐,见下文发布方法);
2. 仓库里提交了构建产物的源码包(`manifest.json` 在仓库根,或 `extensions/<id>/` 布局里恰有一个)。

装完会列出扩展的命令、宿主服务、权限与联网白名单;首次启用仍要在「设置 → 扩展」里同意,同意记录按内容指纹记死,内容变了要重新同意。已装的同名包不覆盖,要覆盖加 `--force`。

## 发布一个扩展

1. 按本地流程做出扩展目录:`lueying ext new <id>` → 实现 → 构建:
   ```sh
   cargo build --release -p <id> --target wasm32-unknown-unknown
   wasm-tools component new target/wasm32-unknown-unknown/release/<id>.wasm -o extensions/<id>/extension.wasm
   ```
   组件文件不超过 16 MiB,清单要求见[扩展总览](README.md)。
2. 打包并检查:`lueying ext pack extensions/<id>` 产出 `<id>.luext`,再 `lueying ext install` 真机跑通。
3. 发到 GitHub:仓库打 tag,并在该 tag 的 GitHub Release 里附上 `<id>.luext`(一个 Release 一个包;资产名用 `<仓库名>.luext` 最稳)。
4. (可选)在本页列表里加一行:发起 PR,改下面的小节即可。

源码包路径是给「不想发 Release」的作者的兜底:把构建好的 `extension.wasm` 与 `manifest.json` 提交进仓库,同样 `add` 得上;仓库里不要混入未构建的半成品。

## 收录列表

| 扩展 | 仓库 | 说明 |
|---|---|---|
| —(虚位以待,发起 PR 收录) | | |
