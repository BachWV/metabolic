"use strict";
(() => {
  const root = document.getElementById("blog-comments");
  if (!root) return;
  const api = root.dataset.api.replace(/\/$/, "");
  const page = root.dataset.page;
  const form = root.querySelector("form"), list = root.querySelector(".bc-list"), status = root.querySelector(".bc-status");
  const reply = root.querySelector(".bc-reply"), previous = root.querySelector(".bc-prev"), next = root.querySelector(".bc-next");
  let parent = null, cursor = 0, loading = false;
  const message = (text) => { status.textContent = text; };
  function node(tag, text, cls) { const n = document.createElement(tag); n.textContent = text; if (cls) n.className = cls; return n; }
  function cancelReply() { parent = null; reply.hidden = true; }
  reply.querySelector("button").onclick = cancelReply;
  async function request(path, options = {}) {
    const response = await fetch(`${api}${path}`, {credentials: "omit", ...options});
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(data.error || `请求失败 (${response.status})`);
    return data;
  }
  function render(comments) {
    list.replaceChildren();
    const byId = new Map(comments.map(c => [c.id, c]));
    const roots = comments.filter(c => !byId.has(c.parent_id)).sort((a, b) => b.created_at - a.created_at || b.id - a.id);
    const group = new Map(roots.map(c => [c.id, []]));
    for (const c of comments) {
      if (!byId.has(c.parent_id)) continue;
      let ancestor = c, seen = new Set();
      while (byId.has(ancestor.parent_id) && !seen.has(ancestor.id)) { seen.add(ancestor.id); ancestor = byId.get(ancestor.parent_id); }
      group.get(ancestor.id)?.push(c);
    }
    function card(c, child) {
      const article = node("article", "", `bc-comment${child ? " bc-child" : ""}`); article.id = `comment-${c.id}`;
      let author = node("strong", c.unavailable ? "评论不可见" : c.nick);
      if (c.website && /^https?:\/\//i.test(c.website)) { author = node("a", c.nick); author.href = c.website; author.rel = "ugc nofollow noopener noreferrer"; author.target = "_blank"; }
      article.append(author);
      if (c.is_admin) article.append(node("span", "博主", "bc-badge"));
      const target = byId.get(c.parent_id);
      article.append(node("div", `${new Date(c.created_at * 1000).toLocaleString()}${target ? ` · 回复 ${target.unavailable ? "已隐藏评论" : target.nick}` : ""}`, "bc-meta"));
      article.append(node("p", c.body));
      if (!c.unavailable) {
        const button = node("button", "回复"); button.type = "button";
        button.onclick = () => { parent = c.id; reply.querySelector("span").textContent = `回复 ${c.nick}`; reply.hidden = false; form.elements.body.focus(); };
        article.append(button);
      }
      return article;
    }
    for (const c of roots) { list.append(card(c, false)); for (const child of group.get(c.id)) list.append(card(child, true)); }
    if (!comments.length) list.append(node("p", "还没有评论，欢迎留下第一条。"));
  }
  async function load() {
    if (loading) return;
    loading = true; previous.disabled = next.disabled = true;
    try {
      const data = await request(`/api/comments?page=${encodeURIComponent(page)}&cursor=${cursor}`);
      render(data.comments); previous.hidden = cursor === 0; next.hidden = !data.has_more;
    } finally { loading = false; previous.disabled = next.disabled = false; }
  }
  form.addEventListener("submit", async (event) => {
    event.preventDefault(); const button = form.querySelector(".bc-submit"); button.disabled = true;
    let saved = false;
    try {
      message("正在发布…");
      const fields = Object.fromEntries(new FormData(form));
      await request("/api/comments", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify({...fields, page, parent_id: parent})});
      saved = true; form.elements.body.value = ""; cancelReply(); cursor = 0;
      await load(); message("评论已发布。回复会显示在对应评论下，较早的讨论可能在后续页面。");
    } catch (e) { message(saved ? "评论已保存，但列表刷新失败，请刷新页面查看。" : `未能发布：${e.message}`); }
    finally { button.disabled = false; }
  });
  previous.onclick = () => { if (loading) return; cursor = Math.max(0, cursor - 1); load().catch(e => message(e.message)); };
  next.onclick = () => { if (loading) return; cursor++; load().catch(e => message(e.message)); };
  load().catch(() => message("评论暂时无法加载，请稍后刷新页面。"));
})();
