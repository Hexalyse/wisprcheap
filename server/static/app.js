"use strict";
document.querySelectorAll("[data-copy]").forEach(button => {
  button.addEventListener("click", async () => {
    const value = document.getElementById(button.dataset.copy)?.textContent || "";
    try {
      if (navigator.clipboard && window.isSecureContext) await navigator.clipboard.writeText(value.trim());
      else {
        const field = document.createElement("textarea");
        field.value = value.trim(); document.body.append(field); field.select();
        const copied = document.execCommand("copy"); field.remove();
        if (!copied) throw new Error("clipboard unavailable");
      }
      button.textContent = "Copied";
    } catch { button.textContent = "Select the text to copy"; }
  });
});
const pairing = document.querySelector("[data-pair-status]");
if (pairing) {
  const feedback = document.getElementById("pair-feedback");
  const expires = Number(pairing.dataset.expires) * 1000;
  let done = false;
  async function check() {
    if (done || document.hidden) return;
    if (Date.now() >= expires) {
      done = true; feedback.textContent = "This code has expired. Create a new code from Devices.";
      feedback.className = "error"; pairing.querySelectorAll(".pair-action").forEach(el => el.remove());
      return;
    }
    feedback.textContent = `Waiting for your device · expires in ${Math.ceil((expires - Date.now()) / 60000)} min`;
    try {
      const response = await fetch(pairing.dataset.pairStatus, {cache: "no-store"});
      if (!response.ok) return;
      const status = await response.json();
      if (status.paired) {
        done = true; feedback.textContent = "Device paired successfully. Open WisprCheap to finish the passphrase setup.";
        feedback.className = "notice"; pairing.querySelectorAll(".pair-action").forEach(el => el.remove());
      } else if (status.expired) {
        done = true; feedback.textContent = "This code has expired. Create a new code from Devices."; feedback.className = "error";
        pairing.querySelectorAll(".pair-action").forEach(el => el.remove());
      }
    } catch { /* A temporary connection failure leaves the readable code available. */ }
  }
  check();
  const timer = setInterval(() => { if (done) clearInterval(timer); else check(); }, 3000);
  document.addEventListener("visibilitychange", check);
}
