# modules/ping.aero 解說

循環引用的一半：`ping` import `pong`，`pong` 又 import 回 `ping`，loader 允許且只載入一次。

## 內容

完整註解程式碼見下方「完整註解程式碼」；`is_even(0)` 為真，否則遞迴呼叫 `pong::is_odd(n-1)`。與 `pong.aero` 互為鏡像，奇偶性在兩檔間來回判定。

## 被誰引用

`main.aero` 呼叫 `ping::is_even(10)`（→`true`）、`ping::is_even(7)`（→`false`）。

## 完整註解程式碼（`ping.aero` 全文）

```aero
// 循環引用的一半：ping import pong，pong 又 import 回 ping。
// loader 允許循環引用且同一檔案只載入一次；合併時符號加前綴區分。
// 被 main.aero 以 ping::is_even 呼叫。

// 引入 pong.aero，別名 pong（路徑相對於本檔，即同目錄）。
import "pong.aero" as pong;

// 判偶數：基例 0 為偶；否則遞減 1 交給 pong::is_odd（兩檔來回互遞迴）。
fn is_even(n: Int) -> Bool
  if n == 0
    return true;
  end
  return pong::is_odd(n - 1);
end
```
