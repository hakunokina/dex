// Ortak kit: anında anahtar. Kaydetme davranışını kontrol türü belirler:
// switch değişince kaydedilir; yazı alanı satır içi Kaydet ister.
export function Switch({ label, checked, disabled, onChange }: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      className="set-switch"
      aria-label={label}
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    />
  );
}
