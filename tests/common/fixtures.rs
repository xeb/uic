pub fn html_for_path(path: &str) -> Option<&'static str> {
    match path {
        "/basic" => Some(BASIC_HTML),
        "/drift_a" => Some(DRIFT_A_HTML),
        "/drift_b" => Some(DRIFT_B_HTML),
        "/modal" => Some(MODAL_HTML),
        "/spa_shadow" => Some(SPA_SHADOW_HTML),
        "/scroll" => Some(SCROLL_HTML),
        "/table" => Some(TABLE_HTML),
        _ => None,
    }
}

const BASIC_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Basic Form</title>
<style>
  body { font-family: sans-serif; padding: 40px; max-width: 520px; }
  label { display: block; margin-top: 12px; font-weight: 600; }
  input, select, button { margin-top: 4px; padding: 8px 12px; font-size: 14px; }
  #status { margin-top: 20px; padding: 12px; background: #ecfdf5; border: 1px solid #10b981; display: none; }
</style>
</head>
<body>
  <h1>Account Setup</h1>
  <form id="f" onsubmit="event.preventDefault(); document.getElementById('status').style.display='block'; document.getElementById('status').textContent='Saved: ' + document.getElementById('email').value + ' (' + document.getElementById('role').value + ', remember=' + document.getElementById('rem').checked + ')';">
    <label for="email">Work Email</label>
    <input id="email" type="email" placeholder="you@company.com" />

    <label for="role">Team Role</label>
    <select id="role">
      <option value="viewer">Viewer</option>
      <option value="editor">Editor</option>
      <option value="admin">Administrator</option>
    </select>

    <label><input id="rem" type="checkbox" /> Remember this device</label>

    <div style="margin-top:16px;">
      <button type="submit">Save Profile</button>
    </div>
  </form>
  <div id="status"></div>
</body>
</html>"#;

const DRIFT_A_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Portal v1</title>
<style>
  body { font-family: sans-serif; padding: 40px; }
  .card { width: 340px; border: 1px solid #ccc; padding: 24px; border-radius: 8px; }
  input { display: block; width: 100%; margin: 8px 0 16px; padding: 8px; }
  button { background: #2563eb; color: #fff; border: none; padding: 10px 18px; border-radius: 6px; cursor: pointer; }
</style>
</head>
<body>
  <div class="card">
    <h2>Sign In</h2>
    <label for="u">Username</label>
    <input id="u" type="text" placeholder="Enter username" />
    <button id="go" onclick="document.getElementById('out').textContent='Welcome ' + document.getElementById('u').value">Sign In</button>
    <p id="out"></p>
  </div>
</body>
</html>"#;

/// Completely different DOM structure, positioning, CSS, tag types (`div[role=button]`), and copy.
const DRIFT_B_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Portal v2 Redesigned</title>
<style>
  body { font-family: Georgia, serif; margin: 0; display: flex; justify-content: flex-end; padding: 80px 100px; background: #0f172a; color: #f8fafc; }
  .panel { width: 380px; background: #1e293b; padding: 32px; border-radius: 16px; }
  .field { width: 100%; padding: 10px; margin: 12px 0 20px; border-radius: 8px; border: 1px solid #475569; }
  .cta { display: inline-block; background: #16a34a; color: white; padding: 12px 24px; border-radius: 999px; cursor: pointer; font-weight: bold; }
</style>
</head>
<body>
  <section class="panel">
    <h1>Member Access</h1>
    <input aria-label="Account handle or username" class="field" placeholder="e.g. jane.doe" />
    <div role="button" tabindex="0" class="cta" onclick="document.getElementById('banner').textContent='Welcome ' + document.querySelector('.field').value">
      Log into your account
    </div>
    <div id="banner" style="margin-top:16px;font-size:18px;"></div>
  </section>
</body>
</html>"#;

const MODAL_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Reports</title>
<style>
  body { font-family: sans-serif; padding: 40px; }
  #overlay {
    position: fixed; inset: 0; background: rgba(0,0,0,0.65);
    display: flex; align-items: center; justify-content: center; z-index: 1000;
  }
  #dialog { background: #fff; padding: 28px; border-radius: 10px; max-width: 420px; }
  button { padding: 10px 16px; font-size: 14px; cursor: pointer; }
</style>
</head>
<body>
  <h1>Quarterly Reports</h1>
  <button id="dl" onclick="document.getElementById('msg').textContent='Report Downloaded!'">Download Report</button>
  <p id="msg"></p>

  <div id="overlay" role="dialog" aria-modal="true">
    <div id="dialog">
      <h2>Cookie Preferences</h2>
      <p>We use cookies to improve your experience. Please accept to continue.</p>
      <button id="accept" onclick="document.getElementById('overlay').style.display='none'">Accept All Cookies</button>
    </div>
  </div>
</body>
</html>"#;

const SPA_SHADOW_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Async Shadow SPA</title>
<style>body { font-family: sans-serif; padding: 40px; }</style>
</head>
<body>
  <h1>Async Workspace</h1>
  <div id="app">Loading workspace...</div>
  <div id="result" style="margin-top:16px;font-weight:bold;"></div>
  <script>
    setTimeout(() => {
      const app = document.getElementById('app');
      app.innerHTML = '';
      const host = document.createElement('div');
      const shadow = host.attachShadow({ mode: 'open' });
      const btn = document.createElement('button');
      btn.textContent = 'Launch Rocket';
      btn.style.padding = '12px 20px';
      btn.style.fontSize = '15px';
      btn.onclick = () => {
        document.getElementById('result').textContent = 'Rocket Launched!';
      };
      shadow.appendChild(btn);
      app.appendChild(host);
    }, 450);
  </script>
</body>
</html>"#;

const SCROLL_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Long Checkout</title>
<style>
  body { font-family: sans-serif; padding: 40px; }
  .spacer { height: 1500px; background: linear-gradient(#f1f5f9, #e2e8f0); padding: 24px; margin: 20px 0; }
  button { padding: 14px 24px; font-size: 16px; background: #16a34a; color: white; border: none; border-radius: 6px; cursor: pointer; }
</style>
</head>
<body>
  <h1>Order Summary</h1>
  <p>Review your items below and scroll to confirm.</p>
  <div class="spacer">Terms and item details...</div>
  <button id="confirm" onclick="document.getElementById('done').textContent='Order Confirmed!'">Confirm Order</button>
  <p id="done" style="font-size:18px;font-weight:bold;"></p>
</body>
</html>"#;

const TABLE_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Team Directory</title>
<style>
  body { font-family: sans-serif; padding: 40px; }
  table { border-collapse: collapse; width: 560px; }
  th, td { border: 1px solid #cbd5e1; padding: 10px 14px; text-align: left; }
  button { padding: 6px 12px; cursor: pointer; }
</style>
</head>
<body>
  <h1>Team Members</h1>
  <table>
    <thead><tr><th>Name</th><th>Role</th><th>Action</th></tr></thead>
    <tbody>
      <tr>
        <td>Alice Nguyen</td><td>Engineer</td>
        <td><button onclick="document.getElementById('log').textContent='Deleted Alice'">Delete</button></td>
      </tr>
      <tr>
        <td>Bob Martinez</td><td>Designer</td>
        <td><button onclick="document.getElementById('log').textContent='Deleted Bob'">Delete</button></td>
      </tr>
      <tr>
        <td>Charlie Patel</td><td>Manager</td>
        <td><button onclick="document.getElementById('log').textContent='Deleted Charlie'">Delete</button></td>
      </tr>
    </tbody>
  </table>
  <p id="log" style="margin-top:20px;font-weight:bold;"></p>
</body>
</html>"#;
