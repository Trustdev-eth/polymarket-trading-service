use axum::response::Html;

fn theme_boot() -> &'static str {
    r#"<script>(function(){var t=localStorage.getItem('oq_theme');if(t!=='light'&&t!=='dark')t='dark';document.documentElement.setAttribute('data-theme',t);var l=localStorage.getItem('oq_lang');if(l!=='zh'&&l!=='en')l='en';document.documentElement.setAttribute('data-lang',l);document.documentElement.setAttribute('lang',l==='zh'?'zh-CN':'en');})();</script>"#
}

fn lang_toggle() -> &'static str {
    r#"<div class="lang-toggle" id="langToggle" role="group" aria-label="Language">
      <button type="button" data-lang="en" class="on">EN</button>
      <button type="button" data-lang="zh">中文</button>
    </div>"#
}

fn theme_toggle() -> &'static str {
    r#"<button type="button" class="theme-toggle" id="themeToggle" aria-label="Switch to light theme" title="Theme">
      <svg class="theme-icon theme-icon-sun" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="5" fill="currentColor"/><path fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" d="M12 2v2.4M12 19.6V22M4.2 4.2l1.7 1.7M18.1 18.1l1.7 1.7M2 12h2.4M19.6 12H22M4.2 19.8l1.7-1.7M18.1 5.9l1.7-1.7"/></svg>
      <svg class="theme-icon theme-icon-moon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M19.2 13.4A7.4 7.4 0 0 1 10.6 4.8 8 8 0 1 0 19.2 13.4z"/></svg>
      <span class="theme-label" id="themeLabel">Light</span>
    </button>"#
}

fn chrome_js() -> &'static str {
    r#"<script src="/i18n.js?v=i18n5"></script>
<script src="/theme.js?v=i18n5"></script>"#
}

fn brand_mark(admin_site: bool) -> &'static str {
    if admin_site {
        r#"<a class="brand" href="/" aria-label="OhioismQuant Admin"><img class="brand-logo" src="/ohioismquant-logo.png" alt=""><span class="brand-name">ohioism<span class="brand-secondary">quant</span> <span class="brand-admin">Admin</span></span></a>"#
    } else {
        r#"<a class="brand" href="/" aria-label="OhioismQuant"><img class="brand-logo" src="/ohioismquant-logo.png" alt=""><span class="brand-name">ohioism<span class="brand-secondary">quant</span></span></a>"#
    }
}

fn nav(
    email: Option<&str>,
    admin: bool,
    admin_href: &str,
    admin_site: bool,
    app_url: &str,
) -> String {
    let brand = brand_mark(admin_site);
    let pages = if admin_site {
        format!(
            r#"<a href="{app}" data-i18n="nav.tradesite">Trade site</a>"#,
            app = app_url
        )
    } else if email.is_some() {
        let admin_link = if admin {
            format!(
                r#"<a href="{h}" data-i18n="nav.admin">Admin</a>"#,
                h = admin_href
            )
        } else {
            String::new()
        };
        format!(
            r#"<a href="/dash" data-i18n="nav.dash">Dashboard</a>
               <a href="/pnl" data-i18n="nav.pnl">PnL</a>
               <a href="/desk" data-i18n="nav.desk">Desk</a>
               {admin_link}"#
        )
    } else {
        r#"<a href="/login" data-i18n="nav.signin">Sign in</a><a class="btn" href="/login?plan=test" data-i18n="nav.start">Start $50 test</a>"#
            .into()
    };
    let logout = if email.is_some() {
        r#"<button class="head-logout" type="button" id="logout" data-i18n="nav.logout">Log out</button>"#
    } else {
        ""
    };
    format!(
        r#"<header class="site-header"><div class="header-inner">
        {brand}
        <nav class="nav head-pages">{pages}</nav>
        <div class="head-grow"></div>
        <nav class="nav nav-end">{lang}{toggle}{logout}</nav>
        </div></header>"#,
        brand = brand,
        pages = pages,
        logout = logout,
        lang = lang_toggle(),
        toggle = theme_toggle()
    )
}

fn shell(title: &str, nav: &str, body: &str, extra: &str, footer: &str) -> Html<String> {
    Html(format!(
        r##"<!doctype html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{boot}
<link rel="stylesheet" href="/app.css?v=hdr2">
<link rel="icon" href="/ohioismquant-logo.png" type="image/png">
</head>
<body>
{nav}
{body}
<footer><div class="wrap">{footer}</div></footer>
{extra}
{chrome}
</body>
</html>"##,
        title = title,
        boot = theme_boot(),
        nav = nav,
        body = body,
        extra = extra,
        footer = footer,
        chrome = chrome_js()
    ))
}

fn trade_footer() -> &'static str {
    r#"<span data-i18n="foot.trade">trade.ohioismquant.vip · your wallet, your fills · not investment advice</span>"#
}

fn notify_modal() -> &'static str {
    r#"<div id="notify" class="modal" hidden>
  <div class="modal-card">
    <h3 id="notifyTitle" data-i18n="notify.title">Notice</h3>
    <p id="notifyBody"></p>
    <button class="btn" type="button" id="notifyOk" data-i18n="notify.ok">OK</button>
  </div>
</div>"#
}

fn app_header(email: &str, admin: bool, admin_href: &str, page: &str) -> String {
    let _ = email;
    let on = |p: &str| if page == p { " on" } else { "" };
    let admin_link = if admin {
        format!(
            r#"<a href="{h}" data-i18n="nav.admin">Admin</a>"#,
            h = admin_href
        )
    } else {
        String::new()
    };
    format!(
        r#"<header class="site-header"><div class="header-inner">
        {brand}
        <nav class="nav head-pages">
          <a class="{d}" href="/dash" data-i18n="nav.dash">Dashboard</a>
          <a class="{p}" href="/pnl" data-i18n="nav.pnl">PnL</a>
          <a class="{s}" href="/desk" data-i18n="nav.desk">Desk</a>
          {admin_link}
        </nav>
        <div class="head-grow"></div>
        <div class="head-ticker">
          <span class="head-chip">BTC 15m</span>
          <div class="head-acct">
            <span><span data-i18n="acct.cash">Cash</span> <b id="acctCash">—</b></span>
            <span><span data-i18n="acct.port">Portfolio</span> <b id="acctPort">—</b></span>
          </div>
          <span class="head-mode" id="headMode">—</span>
        </div>
        <nav class="nav nav-end">
          {lang}
          {toggle}
          <button class="head-logout" type="button" id="logout" data-i18n="nav.logout">Log out</button>
        </nav>
        </div></header>"#,
        d = on("dash"),
        p = on("pnl"),
        s = on("desk"),
        brand = brand_mark(false),
        admin_link = admin_link,
        lang = lang_toggle(),
        toggle = theme_toggle()
    )
}

fn vip_gate(vip: bool) -> String {
    if vip {
        r#"<div id="vipGate" class="vip-gate" hidden></div>"#.into()
    } else {
        r#"<div id="vipGate" class="vip-gate">
  <div class="vip-card">
    <p class="eyebrow" data-i18n="vip.eye">Members only</p>
    <h2 data-i18n="vip.h">Pay to unlock</h2>
    <p data-i18n="vip.p">Dashboard, PnL, paper, and live are for paid traders. Open the desk to subscribe — then this page turns on.</p>
    <div class="btn-row">
      <a class="btn" href="/desk" data-i18n="vip.cta">Pay on the desk</a>
    </div>
  </div>
</div>"#
            .into()
    }
}

fn dash_shell(title: &str, nav: &str, body: &str, scripts: &str) -> Html<String> {
    dash_shell_ex(title, nav, body, scripts, false)
}

fn chart_shell(title: &str, nav: &str, body: &str, scripts: &str) -> Html<String> {
    dash_shell_ex(title, nav, body, scripts, true)
}

fn dash_shell_ex(title: &str, nav: &str, body: &str, scripts: &str, charts: bool) -> Html<String> {
    let chart = if charts {
        r#"<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js"></script>"#
    } else {
        ""
    };
    Html(format!(
        r##"<!doctype html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{boot}
<link rel="stylesheet" href="/app.css?v=hdr2">
<link rel="stylesheet" href="/dash.css?v=hdr1">
<link rel="icon" href="/ohioismquant-logo.png" type="image/png">
{chart}
</head>
<body class="dash-app">
{nav}
{body}
{modal}
{chrome}
{scripts}
</body>
</html>"##,
        title = title,
        boot = theme_boot(),
        chart = chart,
        nav = nav,
        body = body,
        modal = notify_modal(),
        chrome = chrome_js(),
        scripts = scripts
    ))
}

fn admin_footer() -> &'static str {
    r#"<span data-i18n="foot.admin">admin.ohioismquant.vip · operators only · not the course admin</span>"#
}

pub fn landing(email: Option<&str>, admin: bool, admin_href: &str) -> Html<String> {
    let body = r#"
<main>
  <section class="wrap wrap-wide hero">
    <div>
      <p class="eyebrow" data-i18n="land.eye">Polymarket · BTC 15-minute up/down</p>
      <h1 data-i18n="land.h1">A working 15-minute taker. Your wallet. Our model.</h1>
      <p class="lead" data-i18n="land.lead">OhioismQuant Trade is the live BTC 15-minute strategy the operator runs every day. Same quantitative framework as the OhioismQuant course. The model only trades when the market is mispriced. You keep the capital. We host the servers.</p>
      <p class="pull" data-i18n="land.pull">A taker does not print because it “wins a lot.” It prints because the quote is early, the fill is cheap, and the same trade is taken hundreds of times.</p>
      <div class="btn-row" style="margin-top:22px">
        <a class="btn" href="/login?plan=test" data-i18n="land.ctaTest">3-day test · $50</a>
        <a class="btn btn-ghost" href="/login?plan=month" data-i18n="land.ctaMonth">Monthly desk · $800</a>
      </div>
      <p class="note" data-i18n="land.note">The $50 test is not credited toward the $800 month. Minimum size is 5 shares · $200 in the funder.</p>
    </div>
    <aside class="panel expect">
      <p class="eyebrow" data-i18n="land.expectEye">Trader expectations</p>
      <h2 data-i18n="land.expectH">What the book usually looks like</h2>
      <p class="track-lead" data-i18n="land.expectP">Figures below are typical on an active day, on the 5-share starter book, and on the 50-share month cap. They are not a forecast. Quiet windows we skip. Red days happen.</p>
      <dl class="kpi">
        <div><dt data-i18n="land.kpiGreen">Typical green day · 5 shares</dt><dd>+$20 – $100</dd></div>
        <div><dt data-i18n="land.kpiRed">Typical red day · 5 shares</dt><dd class="neg">−$15 – $50</dd></div>
        <div><dt data-i18n="land.kpiMonth">Typical month · 50 shares</dt><dd>+$7k – $12k</dd></div>
        <div><dt data-i18n="land.kpiWin">Win rate when we trade</dt><dd>~55 – 60%</dd></div>
        <div><dt data-i18n="land.kpiRoi">Active-day ROI</dt><dd>~5 – 15% of fund</dd></div>
        <div><dt data-i18n="land.kpiEdge">Edge vs volume</dt><dd>+1.8%</dd></div>
      </dl>
      <p class="note" data-i18n="land.expectNote">Win rate is only for windows we actually trade — most 15-minute windows are no-trade. ROI is versus the $200–$7,000 you actually park, not versus the $800 fee.</p>
    </aside>
  </section>
  <div class="strip">
    <div><b>$200</b><span data-i18n="land.stripFund">MINIMUM FUND</span></div>
    <div><b>5 sh</b><span data-i18n="land.stripSize">EXCHANGE MINIMUM</span></div>
    <div><b>$1,500/mo</b><span data-i18n="land.stripFeed">PAID PRICE FEED</span></div>
    <div><b>UK + TKY</b><span data-i18n="land.stripRail">BRAIN / WIRE</span></div>
  </div>
  <section class="wrap wrap-wide model">
    <p class="eyebrow" data-i18n="land.modelEye">The strategy</p>
    <h2 data-i18n="land.modelH">How a window is actually traded</h2>
    <p class="model-lead" data-i18n="land.modelP">Polymarket lists a BTC up/down contract every 15 minutes. A professional low-latency feed prices that window. If the live market is still off that price, we trade. If not, we wait. That is the whole system.</p>
    <div class="model-grid">
      <article>
        <p class="eyebrow">01</p>
        <h3 data-i18n="land.m1h">Skip most windows</h3>
        <p data-i18n="land.m1p">96 windows a day. The model only trades when the edge is there. A day with three good trades is a real day. A day with none is also normal.</p>
      </article>
      <article>
        <p class="eyebrow">02</p>
        <h3 data-i18n="land.m2h">Small edge, many times</h3>
        <p data-i18n="land.m2p">When we trade, this book wins about 55–60% of the time. The 1.8% of volume is the number that matters — not a vanity 80% win rate. Losers are full 15-minute contracts. That is why size and fund matter.</p>
      </article>
      <article>
        <p class="eyebrow">03</p>
        <h3 data-i18n="land.m3h">PnL scales with shares</h3>
        <p data-i18n="land.m3p">5 shares on $200 is how you sit on the rail. 50 shares on ~$7,000 is how monthly PnL becomes serious. Same signal. Your size. Your wallet.</p>
      </article>
    </div>
  </section>
  <section class="wrap wrap-wide size-sec">
    <div class="size-head">
      <div>
        <p class="eyebrow" data-i18n="land.sizeEye">Size and money</p>
        <h2 data-i18n="land.sizeH">Starter book vs working book</h2>
      </div>
      <p data-i18n="land.sizeP">Read this table before you pay. The $800 month is for traders who intend to run size. The $200 book is to see the same waves, not to replace a salary.</p>
    </div>
    <div class="size-table-wrap">
      <table class="size-table">
        <thead>
          <tr>
            <th data-i18n="land.thBook">Book</th>
            <th data-i18n="land.thFund">Fund</th>
            <th data-i18n="land.thGreen">Typical green day</th>
            <th data-i18n="land.thRed">Typical red day</th>
            <th data-i18n="land.thMonth">Typical month</th>
            <th data-i18n="land.thRoi">Active-day ROI</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td data-i18n="land.rowStart">5 shares · test / floor</td>
            <td>$200</td>
            <td>+$20 – $100</td>
            <td class="neg">−$15 – $50</td>
            <td data-i18n="land.rowStartM">A few hundred when the tape is good</td>
            <td>~5 – 15%</td>
          </tr>
          <tr>
            <td data-i18n="land.rowFull">50 shares · month cap</td>
            <td>$7,000</td>
            <td>+$150 – $400</td>
            <td class="neg">−$150 – $500</td>
            <td>+$7,000 – $12,000</td>
            <td>~5 – 15%</td>
          </tr>
        </tbody>
      </table>
    </div>
    <p class="note" data-i18n="land.sizeNote">Operator live results on this model in 2026: +$143.8K net, $7.9M volume, about $700 / day at professional size, largest window +$2,280. Your account will not match that until size matches. Past months do not promise the next one.</p>
  </section>
  <section class="wrap wrap-wide feed-hl">
    <div class="feed-copy">
      <p class="eyebrow" data-i18n="land.feedEye">Why this desk is not a Telegram signal</p>
      <h2 data-i18n="land.feedH">$1,500 / month professional price feed</h2>
      <p data-i18n="land.feedP">The operator pays for a low-latency BTC feed so Tokyo sees the print before a free public socket. That is the advantage. Without it, a 15-minute taker is late. Late is not this product. The same person teaches the OhioismQuant course and trades this stack live — the course is the theory; this desk is the production rail.</p>
    </div>
    <ul class="feed-list">
      <li data-i18n="land.feed1">Paid low-latency BTC feed — not a free delayed ticker</li>
      <li data-i18n="land.feed2">One model in Tokyo. Live orders leave a UK server, together, on the same wave.</li>
      <li data-i18n="land.feed3">Mainland China cannot win this race. If you are on the mainland you will not taker this yourself. That is why the wire is hosted.</li>
    </ul>
  </section>
  <section class="wrap wrap-wide markets">
    <p class="eyebrow" data-i18n="land.mktEye">Market coverage</p>
    <div class="market-row">
      <article class="market-chip live"><b data-i18n="land.btc">BTC 15m</b><span data-i18n="land.liveNow">Live now</span></article>
      <article class="market-chip soon"><b data-i18n="land.soon5">5m crypto</b><span data-i18n="land.coming">Not enabled</span></article>
      <article class="market-chip soon"><b data-i18n="land.soonEth">15m ETH / SOL / …</b><span data-i18n="land.coming">Not enabled</span></article>
    </div>
    <p class="note" data-i18n="land.mktNote">Only BTC 15-minute up/down is live. We do not list products we are not running.</p>
  </section>
  <section class="wrap wrap-wide">
    <div class="price-grid">
      <article class="price-card">
        <span class="eyebrow" data-i18n="land.testEye">Test</span>
        <h2 data-i18n="land.testH">Three days on the real rail</h2>
        <div class="amt">$50</div>
        <p style="color:var(--muted)" data-i18n="land.testP">72 hours. Same signal as monthly members. Paper as soon as the invoice clears. Live after you paste keys. Unused hours do not roll into the month.</p>
        <ul>
          <li data-i18n="land.test1">Same wave, same brain</li>
          <li data-i18n="land.test2">5-share Polymarket minimum · $200 fund</li>
          <li data-i18n="land.test3">Desk countdown, then convert if you want the month</li>
        </ul>
        <a class="btn" href="/login?plan=test" style="width:100%;margin-top:18px" data-i18n="land.pay50">Start the $50 test</a>
      </article>
      <article class="price-card featured">
        <span class="eyebrow" data-i18n="land.monthEye">Desk</span>
        <h2 data-i18n="land.monthH">30 days in your own funder</h2>
        <div class="amt">$800</div>
        <p style="color:var(--muted)" data-i18n="land.monthP">Direct buy is allowed. Size up to 50 shares. You keep pUSD. We never take custody of a pool.</p>
        <ul>
          <li data-i18n="land.month1">5–50 shares · $200–$7,000 fund</li>
          <li data-i18n="land.month2">Renew stacks 30 days if still active</li>
          <li data-i18n="land.month3">Keys encrypted at rest. Live orders go out from the UK server.</li>
        </ul>
        <a class="btn" href="/login?plan=month" style="width:100%;margin-top:18px" data-i18n="land.pay800">Subscribe $800</a>
      </article>
    </div>
    <div class="grid3">
      <article><h3 data-i18n="land.s1h">1. Subscribe</h3><p data-i18n="land.s1p">NOWPayments USDT. Access opens when the invoice is finished, confirmed, or sending.</p></article>
      <article><h3 data-i18n="land.s2h">2. Paper the twin</h3><p data-i18n="land.s2p">Same signals, simulated fills on the live book. Watch the dashboard before you post keys.</p></article>
      <article><h3 data-i18n="land.s3h">3. Go live</h3><p data-i18n="land.s3p">Paste Polymarket API keys and the wallet private key. Encrypted at rest. Live trading runs from the UK server.</p></article>
    </div>
    <div class="ink">
      <div>
        <p class="eyebrow" data-i18n="land.schoolEye">Course and desk</p>
        <h2 data-i18n="land.schoolH">The operator trades the course stack.</h2>
        <p data-i18n="land.schoolP">ohioismquant.vip remains the school — the 15-minute quantitative framework. This host is only the hosted desk. We do not take your USDT to trade a pool. If you want the theory, take the course. If you want the live rail, subscribe here.</p>
      </div>
      <div>
        <p class="eyebrow" data-i18n="land.latEye">Latency</p>
        <h2 data-i18n="land.latH">The feed and UK execution are the edge.</h2>
        <p data-i18n="land.latP">$1,500/month data plus a UK order. That is why a mainland connection cannot run this taker competitively. You are buying that rail, not a screenshot of yesterday’s PnL.</p>
      </div>
    </div>
    <p class="risk" data-i18n="land.risk">Risk. Binary prediction markets can lose the full fill. A typical red day on 5 shares is tens of dollars; a bad cluster can be worse. A thin market may fill only part of your size, or none this window. Typical ranges above are what this desk sees on active days — they are not a promise that your month will be green.</p>
  </section>
</main>"#;
    shell(
        "OhioismQuant Trade",
        &nav(email, admin, admin_href, false, ""),
        body,
        r#"<script>const q=new URLSearchParams(location.search).get('ref');if(q)localStorage.setItem('trade_ref',q);</script>"#,
        trade_footer(),
    )
}

pub fn login() -> Html<String> {
    let body = r#"
<div class="auth">
  <p class="eyebrow" data-i18n="login.eye">Member</p>
  <h1 style="font-size:2rem;margin:8px 0 4px" data-i18n="login.h">Sign in</h1>
  <p style="color:var(--muted);font-size:.9rem" data-i18n="login.p">Same account for test and monthly. Password 8+ characters.</p>
  <form id="f">
    <label data-i18n="login.email">Email</label><input id="email" type="email" autocomplete="username" required>
    <label data-i18n="login.pass">Password</label><input id="password" type="password" autocomplete="current-password" required>
    <p class="err" id="err"></p>
    <div class="btn-row" style="margin-top:8px">
      <button class="btn" type="submit" id="in" data-i18n="login.in">Sign in</button>
      <button class="btn btn-ghost" type="button" id="up" data-i18n="login.up">Create account</button>
    </div>
  </form>
</div>
<script>
const ref = new URLSearchParams(location.search).get('ref') || localStorage.getItem('trade_ref') || '';
const nextPlan = new URLSearchParams(location.search).get('plan') || '';
const next = new URLSearchParams(location.search).get('next') || '';
async function send(path){
  err.textContent='';
  const body={email:email.value.trim(),password:password.value,referral:ref};
  const r=await fetch(path,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  const d=await r.json().catch(()=>({}));
  if(!r.ok){err.textContent=d.error||r.statusText;return;}
  if(next.startsWith('/') && !next.startsWith('//')){location.href=next;return;}
  if(nextPlan){location.href='/desk?plan='+encodeURIComponent(nextPlan);return;}
  location.href = (d.member && d.member.active) ? '/dash' : '/desk';
}
f.addEventListener('submit',e=>{e.preventDefault();send('/api/auth/login');});
up.onclick=()=>send('/api/auth/signup');
</script>"#;
    shell(
        "Sign in · Trade",
        &nav(None, false, "/admin", false, ""),
        body,
        "",
        trade_footer(),
    )
}

pub fn admin_login(app_url: &str) -> Html<String> {
    let body = r#"
<div class="auth">
  <p class="eyebrow" data-i18n="adlogin.eye">Operators</p>
  <h1 style="font-size:2rem;margin:8px 0 4px" data-i18n="adlogin.h">Admin sign in</h1>
  <p style="color:var(--muted);font-size:.9rem" data-i18n="adlogin.p">admin.ohioismquant.vip · no public signup. Traders use the trade site.</p>
  <form id="f">
    <label data-i18n="login.email">Email</label><input id="email" type="email" autocomplete="username" required>
    <label data-i18n="login.pass">Password</label><input id="password" type="password" autocomplete="current-password" required>
    <p class="err" id="err"></p>
    <div class="btn-row" style="margin-top:8px">
      <button class="btn" type="submit" data-i18n="login.in">Sign in</button>
    </div>
  </form>
</div>
<script>
f.addEventListener('submit', async e => {
  e.preventDefault();
  err.textContent='';
  const r=await fetch('/api/auth/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({email:email.value.trim(),password:password.value})});
  const d=await r.json().catch(()=>({}));
  if(!r.ok){err.textContent=d.error||r.statusText;return;}
  location.href='/';
});
</script>"#;
    shell(
        "Sign in · Admin",
        &nav(None, true, "/", true, app_url),
        body,
        "",
        admin_footer(),
    )
}

pub fn desk(email: &str, admin: bool, admin_href: &str, vip: bool) -> Html<String> {
    let body = format!(
        r#"
<main class="wrap wrap-wide" style="padding:28px 0 56px">
  <p class="eyebrow" data-i18n="desk.eye">Settings</p>
  <h1 style="font-size:2.2rem;margin:6px 0 4px" data-i18n="desk.h">Desk</h1>
  <p style="color:var(--muted)" data-i18n="desk.sub">size, keys, start / stop, pay</p>
  <div class="cta-bar" id="testBar" style="display:none">
    <div><strong data-i18n="desk.testWin">Test window</strong> · <span id="testLeft">—</span> <span data-i18n="desk.left">left. Unused hours do not roll into $800.</span></div>
    <button class="btn" id="payMonth2" type="button" data-i18n="desk.sub800">Subscribe $800</button>
  </div>
  <div class="cta-bar" id="payBar">
    <div data-i18n="desk.payJoin">Pay to join the waves. Paper starts on confirm. Keys stay on the UK server.</div>
    <div class="btn-row">
      <button class="btn" id="payTest" type="button" data-i18n="desk.payTest">$50 test</button>
      <button class="btn btn-ghost" id="payMonth" type="button" data-i18n="desk.payMonth">$800 / 30d</button>
    </div>
  </div>
  <p class="err" id="payErr"></p>
  <div class="metrics">
    <div class="metric"><span data-i18n="desk.plan">Plan</span><b id="plan">—</b></div>
    <div class="metric"><span data-i18n="desk.access">Access</span><b id="left">—</b></div>
    <div class="metric"><span data-i18n="desk.mode">Mode</span><b id="mode">—</b></div>
    <div class="metric"><span data-i18n="desk.paperCash">Paper cash</span><b id="paperCash">—</b></div>
  </div>
  <div class="desk-grid desk-grid-2{locked}" id="deskMain">
    <section class="panel">
      <p class="eyebrow" data-i18n="desk.sizeEye">Your size</p>
      <h2 style="font-size:1.25rem;margin:6px 0 8px" data-i18n="desk.sizeH">Betting size</h2>
      <p class="note" id="bestLine" data-i18n="desk.sizeNote">Polymarket minimum is 5 shares / $200 fund. Empty size saves as 5.</p>
      <label data-i18n="desk.sizeLab">Betting size (shares / take)</label>
      <input id="setBase" type="number" min="5" step="1" placeholder="5">
      <label data-i18n="desk.fund">Necessary fund</label>
      <p class="fund-readout" id="needFund">$200</p>
      <label class="check-row"><input id="setHedge" type="checkbox" checked> <span data-i18n="desk.hedge">Hedge leftover (flatten after the take)</span></label>
      <div class="btn-row" style="margin-top:12px">
        <button class="btn" id="saveParams" type="button" data-i18n="desk.saveSize">Save my size</button>
      </div>
      <p class="note" style="margin-top:10px" data-i18n="desk.sizeZh">Minimum 5 shares, $200 fund.</p>
      <p class="eyebrow" style="margin-top:22px" data-i18n="desk.run">Run</p>
      <p class="note" id="feedNote"></p>
      <div class="mode-stack">
        <div class="mode-card" id="paperCard">
          <p class="eyebrow" data-i18n="desk.paper">Paper mode</p>
          <p class="mode-status off" id="paperStatus">Stopped</p>
          <div class="btn-row" style="margin-top:10px">
            <button class="btn" id="paperStart" type="button" data-i18n="desk.start">Start</button>
            <button class="btn btn-ghost" id="paperStop" type="button" data-i18n="desk.stop">Stop</button>
          </div>
          <p class="note" id="paperHint"></p>
        </div>
        <div class="mode-card" id="liveCard">
          <p class="eyebrow" data-i18n="desk.live">Live mode</p>
          <p class="mode-status off" id="liveStatus">Stopped</p>
          <div class="btn-row" style="margin-top:10px">
            <button class="btn" id="liveStart" type="button" data-i18n="desk.start">Start</button>
            <button class="btn btn-ghost" id="liveStop" type="button" data-i18n="desk.stop">Stop</button>
          </div>
          <p class="note" id="liveHint"></p>
        </div>
      </div>
      <a class="btn" href="/dash" id="openDash" style="margin-top:14px;width:100%" data-i18n="desk.openDash">Open dashboard</a>
    </section>
    <section class="panel">
      <p class="eyebrow" data-i18n="desk.alerts">Alerts</p>
      <div class="btn-row" style="margin:8px 0 12px">
        <button class="btn btn-ghost btn-sm" id="enablePush" type="button" data-i18n="desk.push">Browser alerts</button>
        <button class="btn btn-ghost btn-sm" id="clearNotes" type="button" data-i18n="desk.clear">Clear</button>
      </div>
      <ul class="note-list" id="noteList"></ul>
      <p class="eyebrow" style="margin-top:22px" data-i18n="desk.keysEye">Polymarket API keys</p>
      <p id="keysStatus" class="note">No keys</p>
      <label data-i18n="desk.funder">Funder address</label><input id="funder" placeholder="0x…">
      <label data-i18n="desk.apiKey">L2 API key</label><input id="apiKey" autocomplete="off">
      <label data-i18n="desk.secret">Secret</label><input id="secret" type="password" autocomplete="new-password">
      <label data-i18n="desk.pass">Passphrase</label><input id="passphrase" type="password" autocomplete="new-password">
      <label data-i18n="desk.pk">Wallet private key</label><input id="privateKey" type="password" autocomplete="new-password" placeholder="0x…">
      <label data-i18n="desk.sig">Signature type</label>
      <select id="sigType"><option value="0">EOA (MetaMask)</option><option value="1">POLY_PROXY (email / Magic)</option><option value="2">GNOSIS_SAFE</option></select>
      <button class="btn" id="saveKeys" type="button" style="width:100%;margin-top:12px" data-i18n="desk.saveKeys">Save encrypted keys</button>
      <p class="note" data-i18n="desk.keysNote">From Polymarket → Settings → API keys. L2 API key, secret, passphrase, and private key are encrypted at rest. Live orders go out from the UK server. We never show the private key again.</p>
    </section>
  </div>
</main>"#,
        locked = if vip { "" } else { " is-locked" }
    );
    dash_shell(
        "Desk · Trade",
        &app_header(email, admin, admin_href, "desk"),
        &body,
        r#"<script src="/desk.js?v=hdr1"></script>"#,
    )
}

pub fn dash(email: &str, admin: bool, admin_href: &str, vip: bool) -> Html<String> {
    let body = format!(
        r#"
<main class="dash-main">
  {gate}
  <div class="run-banner stopped" id="runBanner" data-i18n-html="banner.stop">Stopped · start paper or live on the <a href="/desk">desk</a>.</div>
  <div class="dash-stats" id="stats"></div>
  <div class="dash-meta" id="meta"></div>
  <section class="chart-pane"><h3 data-i18n="dash.price">Price</h3><canvas id="cBtc"></canvas></section>
  <section class="chart-pane"><h3 data-i18n="dash.yn">Yes / No</h3><canvas id="cYn"></canvas></section>
  <section class="chart-pane"><h3 data-i18n="dash.sig">Signal Ŝ</h3><canvas id="cSig"></canvas></section>
  <section class="heat-wrap" style="margin-top:16px">
    <h3 data-i18n="dash.fills">Recent fills</h3>
    <table class="day-table"><thead><tr><th data-i18n="dash.utc">UTC</th><th data-i18n="dash.kind">Kind</th><th data-i18n="dash.tok">Tok</th><th data-i18n="dash.sz">Sz</th><th data-i18n="dash.px">Px</th><th></th></tr></thead><tbody id="fillRows"></tbody></table>
  </section>
</main>"#,
        gate = vip_gate(vip)
    );
    chart_shell(
        "Dashboard · Trade",
        &app_header(email, admin, admin_href, "dash"),
        &body,
        r#"<script src="/dash.js?v=hdr1"></script>"#,
    )
}

pub fn pnl(email: &str, admin: bool, admin_href: &str, vip: bool) -> Html<String> {
    let body = format!(
        r#"
<main class="dash-main">
  {gate}
  <div class="heat-head">
    <div>
      <p class="eyebrow" data-i18n="pnl.eye">PnL</p>
      <h2 data-i18n="pnl.h">BTC 15m windows</h2>
    </div>
    <p><span data-i18n="pnl.cash">Paper cash</span> <b id="pnlCash">—</b></p>
  </div>
  <div class="pnl-grid">
    <section class="heat-wrap">
      <div id="heat"></div>
      <p class="note" data-i18n="pnl.heat">Click a day. Green is net positive windows, red is negative.</p>
    </section>
    <section class="heat-wrap">
      <h3 id="dayLabel">—</h3>
      <table class="day-table"><thead><tr><th data-i18n="dash.utc">UTC</th><th data-i18n="pnl.slug">Slug</th><th data-i18n="nav.pnl">PnL</th><th></th></tr></thead><tbody id="dayRows"></tbody></table>
    </section>
  </div>
  <section class="chart-pane" style="margin-top:16px;height:220px"><h3 data-i18n="pnl.equity">Equity</h3><canvas id="cEq"></canvas></section>
  <section class="heat-wrap" style="margin-top:16px">
    <h3 data-i18n="pnl.fills">Fills</h3>
    <table class="day-table"><thead><tr><th data-i18n="dash.utc">UTC</th><th data-i18n="dash.kind">Kind</th><th data-i18n="dash.tok">Tok</th><th data-i18n="dash.sz">Sz</th><th data-i18n="dash.px">Px</th><th></th></tr></thead><tbody id="fillRows"></tbody></table>
  </section>
</main>"#,
        gate = vip_gate(vip)
    );
    chart_shell(
        "PnL · Trade",
        &app_header(email, admin, admin_href, "pnl"),
        &body,
        r#"<script src="/dash.js?v=hdr1"></script>"#,
    )
}

pub fn admin(email: &str, app_url: &str) -> Html<String> {
    let body = r#"
<main class="wrap wrap-wide" style="padding:28px 0 56px">
  <p class="eyebrow" data-i18n="admin.eye">Admin · admin.ohioismquant.vip</p>
  <h1 style="font-size:2.2rem;margin:6px 0 4px" data-i18n="admin.h">Trade Card</h1>
  <p style="color:var(--muted)" data-i18n="admin.p">Live vs test, partner 15% on $50 and $800, gift days, halt. Not the course site.</p>
  <p class="err" id="admErr"></p>
  <p class="admin-ok" id="admOk"></p>
  <div class="metrics" id="stats"></div>
  <div class="metrics" id="money"></div>
  <section class="panel" style="margin:12px 0">
    <p class="eyebrow" data-i18n="admin.house">House reference</p>
    <div class="metrics" id="house"></div>
  </section>
  <nav class="admin-tabs" id="tabs">
    <button type="button" class="active" data-tab="traders" data-i18n="admin.traders">Traders</button>
    <button type="button" data-tab="payments" data-i18n="admin.payments">Payments</button>
  </nav>
  <section class="panel" id="tab-traders">
    <form id="gift" class="admin-toolbar">
      <div style="flex:1;min-width:180px"><label data-i18n="admin.gift">Gift access</label><input id="giftEmail" placeholder="email"></div>
      <div style="width:100px"><label data-i18n="admin.days">Days</label><input id="giftDays" type="number" value="3" min="1" max="365"></div>
      <button class="btn" type="submit" data-i18n="admin.grant">Grant</button>
      <button class="btn btn-danger" type="button" id="haltAll" data-i18n="admin.halt">Halt all</button>
    </form>
    <div class="admin-toolbar">
      <input id="q" data-i18n-placeholder="admin.search" placeholder="Search email or referral">
      <div class="admin-filters" id="filters">
        <button type="button" class="active" data-filter="all" data-i18n="admin.all">All</button>
        <button type="button" data-filter="test" data-i18n="admin.test">Test</button>
        <button type="button" data-filter="month" data-i18n="admin.month">Month</button>
        <button type="button" data-filter="live" data-i18n="admin.live">Live</button>
        <button type="button" data-filter="paper" data-i18n="admin.paper">Paper</button>
        <button type="button" data-filter="halted" data-i18n="admin.halted">Halted</button>
        <button type="button" data-filter="keys" data-i18n="admin.keys">Keys</button>
        <button type="button" data-filter="expiring" data-i18n="admin.expiring">Expiring</button>
      </div>
    </div>
    <div class="admin-table-wrap">
      <table>
        <thead>
          <tr>
            <th data-i18n="admin.email">Email</th><th data-i18n="admin.plan">Plan</th><th data-i18n="admin.until">Until</th><th data-i18n="admin.mode">Mode</th>
            <th data-i18n="admin.keys">Keys</th><th data-i18n="admin.paid">Paid</th><th data-i18n="admin.cut">15%</th><th data-i18n="admin.ref">Ref</th><th></th>
          </tr>
        </thead>
        <tbody id="rows"></tbody>
      </table>
    </div>
  </section>
  <section class="panel" id="tab-payments" hidden>
    <div class="admin-table-wrap">
      <table>
        <thead>
          <tr>
            <th data-i18n="admin.when">When</th><th data-i18n="admin.email">Email</th><th data-i18n="admin.plan">Plan</th><th data-i18n="admin.amount">Amount</th>
            <th data-i18n="admin.cut">15%</th><th data-i18n="admin.status">Status</th><th data-i18n="admin.ref">Ref</th><th></th>
          </tr>
        </thead>
        <tbody id="payRows"></tbody>
      </table>
    </div>
  </section>
</main>
<script src="/admin.js?v=i18n1"></script>"#;
    shell(
        "Admin · Trade Card",
        &nav(Some(email), true, "/", true, app_url),
        body,
        "",
        admin_footer(),
    )
}

pub fn css() -> &'static str {
    include_str!("../static/app.css")
}
pub fn desk_js() -> &'static str {
    include_str!("../static/desk.js")
}
pub fn admin_js() -> &'static str {
    include_str!("../static/admin.js")
}
pub fn dash_css() -> &'static str {
    include_str!("../static/dash.css")
}
pub fn dash_js() -> &'static str {
    include_str!("../static/dash.js")
}
pub fn theme_js() -> &'static str {
    include_str!("../static/theme.js")
}
pub fn i18n_js() -> &'static str {
    include_str!("../static/i18n.js")
}
pub fn logo() -> &'static [u8] {
    include_bytes!("../static/ohioismquant-logo.png")
}
