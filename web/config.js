// Deployment this page talks to. Change CONTRACT after deploying your own copy.
export const CONTRACT = "0x762647adc6541a67251e3a8883ca6d89a7c76156";
export const REPO_URL = "https://github.com/brossXnft/genlayer-sla-escrow";
// Message-fee budget attached to calls that send GEN out (report, withdraw).
// Without it GenLayer does not execute the transfer. Unused budget is refunded.
export const MSG_FEES = 10n ** 16n; // 0.01 GEN
