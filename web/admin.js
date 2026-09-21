"use strict";
(() => {
  const $ = (id) => document.getElementById(id);
  let csrf = sessionStorage.getItem("comments-csrf") || "";
  let offset = 0;
  const message = (text) => { $("message").textContent = text; };
  async function request(path, method = "GET", body) {
    const response = await fetch(path, {method, credentials: "same-origin", headers: {"Content-Type": "application/json", "X-CSRF-Token": csrf}, body: body === undefined ? undefined : JSON.stringify(body)});
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      if (response.status === 401) { $("panel").hidden = true; $("login").hidden = false; }
      throw new Error(data.error || `请求失败 (${response.status})`);
    }
    return data;
  }
  function el(tag, text, cls) { const e = document.createElement(tag); e.textContent = text; if (cls) e.className = cls; return e; }
  function button(text, action) {
    const b = el("button", text); b.type = "button";
    b.addEventListener("click", async () => { b.disabled = true; try { message(""); await action(); } catch (e) { message(e.message); } finally { b.disabled = false; } });
    return b;
  }
  async function load() {
    const data = await request(`/api/admin/comments?offset=${offset}`);
    $("panel").hidden = false; $("login").hidden = true;
    $("mail-status").textContent = data.failed_mail_jobs ? `${data.failed_mail_jobs} 个邮件任务发送失败，请检查 SMTP 后运行 retry-mail。` : "邮件任务无最终失败记录。";
    $("comments").replaceChildren();
    for (const c of data.comments) {
      const card = document.createElement("article");
      card.append(el("strong", `${c.nick || "已删除"}${c.is_admin ? " · 博主" : ""}`));
      card.append(el("div", `#${c.id} · ${c.status} · ${new Date(c.created_at * 1000).toLocaleString()} · ${c.page}`, "meta"));
      if (c.parent_id) card.append(el("div", `回复 #${c.parent_id}`, "meta"));
      if (c.email) card.append(el("div", c.email, "meta"));
      card.append(el("p", c.body));
      if (c.migration_note) card.append(el("p", c.migration_note, "meta"));
      if (c.status === "held") card.append(el("p", "待迁移映射：在映射文件中指定文章路径后重新导入。"));
      if (!["held", "deleted"].includes(c.status)) {
        const target = c.status === "hidden" ? "published" : "hidden";
        card.append(button(target === "published" ? "恢复" : "隐藏", async () => { await request(`/api/admin/comments/${c.id}`, "PATCH", {status: target}); await load(); }));
        card.append(button("删除", async () => {
          if (!confirm("永久清除这条评论的正文和作者信息？回复关系会保留。")) return;
          await request(`/api/admin/comments/${c.id}`, "PATCH", {status: "deleted"}); await load();
        }));
      }
      if (c.status === "published") {
        const form = document.createElement("form");
        const label = el("label", `回复 #${c.id}`);
        const field = document.createElement("textarea"); field.required = true; field.maxLength = 5000; label.append(field);
        const submit = el("button", "以博主身份回复"); form.append(label, submit);
        form.addEventListener("submit", async (event) => {
          event.preventDefault(); submit.disabled = true;
          try { await request(`/api/admin/comments/${c.id}/reply`, "POST", {body: field.value}); message("回复已发布"); await load(); }
          catch (e) { message(e.message); } finally { submit.disabled = false; }
        }); card.append(form);
      }
      $("comments").append(card);
    }
    if (!data.comments.length) $("comments").append(el("p", "暂无评论"));
    $("previous").disabled = offset === 0; $("next").disabled = !data.has_more;
  }
  $("login").addEventListener("submit", async (event) => {
    event.preventDefault(); const submit = event.currentTarget.querySelector("button"); submit.disabled = true;
    try { const data = await request("/api/admin/login", "POST", {password: $("login").elements.password.value}); csrf = data.csrf; sessionStorage.setItem("comments-csrf", csrf); $("login").reset(); message(""); await load(); }
    catch (e) { message(e.message); } finally { submit.disabled = false; }
  });
  $("logout").onclick = async () => { try { await request("/api/admin/logout", "POST"); sessionStorage.removeItem("comments-csrf"); csrf = ""; $("panel").hidden = true; $("login").hidden = false; } catch (e) { message(e.message); } };
  $("refresh").onclick = () => load().catch(e => message(e.message));
  $("previous").onclick = () => { offset = Math.max(0, offset - 50); load().catch(e => message(e.message)); };
  $("next").onclick = () => { offset += 50; load().catch(e => message(e.message)); };
  if (csrf) load().catch(e => message(e.message));
})();
