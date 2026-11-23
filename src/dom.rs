use serde::{Deserialize, Serialize};

use crate::cdp::CdpClient;
use crate::error::{Result, UicError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub cx: f64,
    pub cy: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionableElement {
    pub index: usize,
    pub id: String,
    pub tag: String,
    pub role: String,
    #[serde(default)]
    pub input_type: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub in_shadow_dom: bool,
    pub rect: ElementRect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub url: String,
    pub title: String,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub scroll_y: f64,
    pub scroll_height: f64,
    pub can_scroll_down: bool,
    pub can_scroll_up: bool,
    pub elements: Vec<ActionableElement>,
}

impl ActionableElement {
    /// Concise, high-signal description used in the Decisions API `criteria` map.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        let kind = if !self.role.is_empty() && self.role != self.tag {
            format!("{}/{}", self.tag, self.role)
        } else if !self.input_type.is_empty() {
            format!("{}[{}]", self.tag, self.input_type)
        } else {
            self.tag.clone()
        };
        parts.push(format!("[{}] {}", self.index, kind));

        if !self.label.is_empty() {
            parts.push(format!("\"{}\"", self.label));
        }
        if !self.placeholder.is_empty() && self.placeholder != self.label {
            parts.push(format!("placeholder=\"{}\"", self.placeholder));
        }
        if !self.value.is_empty() && self.value != self.label {
            parts.push(format!("value=\"{}\"", self.value));
        }
        if let Some(c) = self.checked {
            parts.push(if c { "checked" } else { "unchecked" }.to_string());
        }
        if !self.options.is_empty() {
            let opts: Vec<&str> = self.options.iter().take(8).map(|s| s.as_str()).collect();
            parts.push(format!("options=[{}]", opts.join(", ")));
        }
        if !self.context.is_empty() {
            parts.push(format!("in: {}", self.context));
        }
        parts.join(" ")
    }
}

/// Single-eval JS extractor that walks DOM + open Shadow Roots, filters invisible/occluded
/// elements, extracts accessible labels + row/section context, and tags each element with a
/// transient registry handle (`window.__uic_els`) for direct focus/select operations.
const EXTRACT_ACTIONABLE_JS: &str = r#"(() => {
  const vw = window.innerWidth || document.documentElement.clientWidth || 1280;
  const vh = window.innerHeight || document.documentElement.clientHeight || 800;
  const INTERACTIVE_ROLES = new Set([
    'button','link','tab','menuitem','menuitemcheckbox','menuitemradio',
    'checkbox','radio','switch','combobox','textbox','searchbox','option','slider'
  ]);
  const INTERACTIVE_TAGS = new Set(['a','button','input','select','textarea','summary']);

  const clean = (s, max = 90) => {
    if (!s) return '';
    return String(s).replace(/\s+/g, ' ').trim().slice(0, max);
  };

  function collectRoots(root, out) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
    let node = walker.nextNode();
    while (node) {
      out.push({ el: node, shadow: root !== document });
      if (node.shadowRoot) {
        collectRoots(node.shadowRoot, out);
      }
      node = walker.nextNode();
    }
  }

  function deepElementFromPoint(x, y) {
    let el = document.elementFromPoint(x, y);
    while (el && el.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === el) break;
      el = inner;
    }
    return el;
  }

  function isInteractive(el) {
    const tag = el.tagName.toLowerCase();
    if (tag === 'input' && (el.type || '').toLowerCase() === 'hidden') return false;
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') return false;
    const role = (el.getAttribute('role') || '').toLowerCase();
    if (INTERACTIVE_TAGS.has(tag)) {
      if (tag === 'a' && !el.hasAttribute('href') && !el.onclick && role !== 'button' && role !== 'link') {
        return false;
      }
      return true;
    }
    if (INTERACTIVE_ROLES.has(role)) return true;
    if (el.isContentEditable) return true;
    if (el.hasAttribute('onclick')) return true;
    const ti = el.getAttribute('tabindex');
    if (ti !== null && ti !== '' && parseInt(ti, 10) >= 0) return true;

    const style = window.getComputedStyle(el);
    if (style.cursor === 'pointer') {
      // Avoid giant wrapper containers with cursor:pointer
      const rect = el.getBoundingClientRect();
      if (rect.width * rect.height < vw * vh * 0.35 && el.children.length <= 4) {
        return true;
      }
    }
    return false;
  }

  function isVisibleAndUnoccluded(el, rect) {
    if (rect.width < 4 || rect.height < 4) return false;
    if (rect.bottom <= 0 || rect.top >= vh || rect.right <= 0 || rect.left >= vw) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || parseFloat(style.opacity) === 0) {
      return false;
    }
    const cx = Math.min(Math.max(rect.left + rect.width / 2, 1), vw - 1);
    const cy = Math.min(Math.max(rect.top + rect.height / 2, 1), vh - 1);
    const hit = deepElementFromPoint(cx, cy);
    if (!hit) return true;
    if (hit === el || el.contains(hit) || hit.contains(el)) return true;
    // Also check label[for] covering an input
    if (hit.tagName && hit.tagName.toLowerCase() === 'label' && el.id && hit.htmlFor === el.id) {
      return true;
    }
    return false;
  }

  function getLabel(el) {
    const aria = el.getAttribute('aria-label');
    if (aria) return clean(aria);
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
      const txt = labelledBy.split(/\s+/).map(id => {
        const ref = (el.getRootNode() || document).getElementById?.(id) || document.getElementById(id);
        return ref ? ref.textContent : '';
      }).join(' ');
      if (clean(txt)) return clean(txt);
    }
    if (el.labels && el.labels.length > 0) {
      const txt = Array.from(el.labels).map(l => l.textContent).join(' ');
      if (clean(txt)) return clean(txt);
    }
    const tag = el.tagName.toLowerCase();
    if (tag === 'input' && (el.type === 'submit' || el.type === 'button' || el.type === 'reset')) {
      if (clean(el.value)) return clean(el.value);
    }
    const inner = clean(el.innerText || el.textContent);
    if (inner) return inner;
    const ph = clean(el.getAttribute('placeholder'));
    if (ph) return ph;
    const title = clean(el.getAttribute('title') || el.getAttribute('alt') || el.getAttribute('name'));
    if (title) return title;
    return '';
  }

  function getContext(el) {
    const row = el.closest('tr, [role="row"], li, [role="dialog"], dialog, form, article, section');
    if (!row || row === el) return '';
    if (row.tagName.toLowerCase() === 'tr' || row.getAttribute('role') === 'row') {
      return clean(row.innerText || row.textContent, 80);
    }
    const heading = row.querySelector('h1, h2, h3, h4, legend, [role="heading"]');
    if (heading && !el.contains(heading)) {
      return clean(heading.textContent, 60);
    }
    return '';
  }

  const all = [];
  collectRoots(document, all);

  const selected = [];
  const chosenSet = new Set();

  for (const item of all) {
    const el = item.el;
    if (!isInteractive(el)) continue;
    const rect = el.getBoundingClientRect();
    if (!isVisibleAndUnoccluded(el, rect)) continue;

    // Skip if an already-selected ancestor is a native button/link wrapping this span/div
    let skip = false;
    let p = el.parentElement;
    while (p) {
      if (chosenSet.has(p)) {
        const ptag = p.tagName.toLowerCase();
        if (ptag === 'button' || ptag === 'a') {
          skip = true;
          break;
        }
      }
      p = p.parentElement;
    }
    if (skip) continue;

    chosenSet.add(el);
    selected.push({ el, rect, shadow: item.shadow });
  }

  window.__uic_els = {};
  const elements = selected.map((item, idx) => {
    const el = item.el;
    const r = item.rect;
    const id = 'e' + (idx + 1);
    window.__uic_els[id] = el;
    const tag = el.tagName.toLowerCase();
    const role = (el.getAttribute('role') || '').toLowerCase();
    const input_type = tag === 'input' ? (el.type || 'text').toLowerCase() : '';
    const checked = (tag === 'input' && (input_type === 'checkbox' || input_type === 'radio'))
      ? !!el.checked
      : (el.hasAttribute('aria-checked') ? el.getAttribute('aria-checked') === 'true' : null);
    const options = tag === 'select'
      ? Array.from(el.options || []).map(o => clean(o.textContent || o.value, 50))
      : [];
    const value = (tag === 'input' && input_type !== 'password' && input_type !== 'checkbox' && input_type !== 'radio')
      || tag === 'textarea' || tag === 'select'
      ? clean(el.value || '', 60)
      : '';

    return {
      index: idx + 1,
      id,
      tag,
      role,
      input_type,
      label: getLabel(el),
      placeholder: clean(el.getAttribute('placeholder') || '', 60),
      value,
      context: getContext(el),
      checked,
      options,
      in_shadow_dom: item.shadow,
      rect: {
        x: r.left,
        y: r.top,
        width: r.width,
        height: r.height,
        cx: r.left + r.width / 2,
        cy: r.top + r.height / 2,
      }
    };
  });

  const scrollY = window.scrollY || document.documentElement.scrollTop || 0;
  const scrollHeight = Math.max(
    document.body ? document.body.scrollHeight : 0,
    document.documentElement ? document.documentElement.scrollHeight : 0
  );

  return {
    url: window.location.href,
    title: document.title || '',
    viewport_width: vw,
    viewport_height: vh,
    scroll_y: scrollY,
    scroll_height: scrollHeight,
    can_scroll_down: scrollY + vh < scrollHeight - 10,
    can_scroll_up: scrollY > 10,
    elements
  };
})()"#;

pub async fn extract_snapshot(cdp: &CdpClient) -> Result<PageSnapshot> {
    let val = cdp.evaluate(EXTRACT_ACTIONABLE_JS).await?;
    let mut snap = serde_json::from_value::<PageSnapshot>(val)
        .map_err(|e| UicError::Dom(format!("failed to parse DOM snapshot: {e}")))?;

    // Also extract actionable elements from any visible cross-origin iframes (e.g. Apple ID / Stripe)
    let iframes = cdp.attach_iframe_targets().await;
    for (sid, ox, oy) in iframes {
        if let Ok(iframe_val) = cdp
            .evaluate_in_session(Some(&sid), EXTRACT_ACTIONABLE_JS)
            .await
        {
            if let Ok(iframe_snap) = serde_json::from_value::<PageSnapshot>(iframe_val) {
                for mut el in iframe_snap.elements {
                    el.rect.x += ox;
                    el.rect.y += oy;
                    el.rect.cx += ox;
                    el.rect.cy += oy;
                    if el.rect.cx >= 0.0
                        && el.rect.cy >= 0.0
                        && el.rect.cx <= snap.viewport_width
                        && el.rect.cy <= snap.viewport_height
                    {
                        let next_idx = snap.elements.len() + 1;
                        el.index = next_idx;
                        el.id = format!("e{next_idx}");
                        snap.elements.push(el);
                    }
                }
            }
        }
    }

    Ok(snap)
}

/// Focuses an element by its transient `__uic_els[id]` handle and optionally clears its value.
pub async fn focus_and_prepare_input(cdp: &CdpClient, id: &str, clear: bool) -> Result<()> {
    let id_json = serde_json::to_string(id).unwrap();
    let js = format!(
        r#"(() => {{
            const el = window.__uic_els && window.__uic_els[{id_json}];
            if (!el) return false;
            el.scrollIntoView({{ block: 'center', inline: 'center' }});
            el.focus();
            if ({clear}) {{
                if ('value' in el) {{
                    const proto = Object.getPrototypeOf(el);
                    const desc = Object.getOwnPropertyDescriptor(proto, 'value');
                    if (desc && desc.set) desc.set.call(el, '');
                    else el.value = '';
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                }} else if (el.isContentEditable) {{
                    el.textContent = '';
                }}
            }}
            return true;
        }})()"#
    );
    let ok = cdp.evaluate(&js).await?.as_bool().unwrap_or(false);
    if !ok {
        return Err(UicError::Dom(format!("element handle {id} no longer exists")));
    }
    Ok(())
}

/// Selects an option on a native `<select>` element by matching text or value (case-insensitive).
pub async fn select_native_option(cdp: &CdpClient, id: &str, option_text: &str) -> Result<bool> {
    let id_json = serde_json::to_string(id).unwrap();
    let opt_json = serde_json::to_string(option_text).unwrap();
    let js = format!(
        r#"(() => {{
            const el = window.__uic_els && window.__uic_els[{id_json}];
            if (!el || el.tagName.toLowerCase() !== 'select') return false;
            const target = {opt_json}.toLowerCase().trim();
            let matched = Array.from(el.options).find(o =>
                o.textContent.toLowerCase().trim() === target ||
                o.value.toLowerCase().trim() === target
            );
            if (!matched) {{
                matched = Array.from(el.options).find(o =>
                    o.textContent.toLowerCase().includes(target) ||
                    o.value.toLowerCase().includes(target)
                );
            }}
            if (!matched) return false;
            el.value = matched.value;
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }})()"#
    );
    Ok(cdp.evaluate(&js).await?.as_bool().unwrap_or(false))
}
