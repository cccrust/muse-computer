# modules/pong.aero 解說

循環引用的另一半：與 `ping.aero` 互 import，證明 loader 可處理循環依賴。

## 內容

完整註解程式碼見下方「完整註解程式碼」；`is_odd(0)` 為假，否則回呼 `ping::is_even(n-1)`。偶數次互遞迴後必落在某一檔的 `n == 0` 基例而終止。

## 被誰引用

不被 `main.aero` 直接引用，經由 `ping::is_even` 間接呼叫（`pong::is_odd`）。

## 完整註解程式碼（`pong.aero` 全文）

```aero
// 循環引用的另一半：與 ping.aero 互 import，證明 loader 可處理循環依賴。
// 不被 main.aero 直接引用，經由 ping::is_even 間接呼叫（pong::is_odd）。

// 引入 ping.aero，別名 ping（與 ping.aero 的 import 方向相反，形成循環）。
import "ping.aero" as ping;

// 判奇數：基例 0 非奇；否則遞減 1 交回 ping::is_even。
// 偶數次互遞迴後必落在某一檔的 n == 0 基例而終止。
fn is_odd(n: Int) -> Bool
  if n == 0
    return false;
  end
  return ping::is_even(n - 1);
end
```
