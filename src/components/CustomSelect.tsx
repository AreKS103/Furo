import { useState, useRef, useEffect } from "react";

interface Option {
  value: string;
  label: string;
}

interface CustomSelectProps {
  value: string;
  onChange: (value: string) => void;
  options: Option[];
  className?: string;
}

export default function CustomSelect({
  value,
  onChange,
  options,
  className = "",
}: CustomSelectProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const selected = options.find((o) => o.value === value);

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className={`${className} flex cursor-pointer items-center justify-between`}
      >
        <span className="truncate">{selected?.label ?? value}</span>
        <svg
          className={`ml-2 h-4 w-4 shrink-0 text-warm-400 transition dark:text-zinc-500 ${
            open ? "rotate-180" : ""
          }`}
          viewBox="0 0 20 20"
          fill="currentColor"
        >
          <path
            fillRule="evenodd"
            d="M5.23 7.21a.75.75 0 011.06.02L10 11.168l3.71-3.938a.75.75 0 111.08 1.04l-4.25 4.5a.75.75 0 01-1.08 0l-4.25-4.5a.75.75 0 01.02-1.06z"
            clipRule="evenodd"
          />
        </svg>
      </button>

      {open && (
        <ul className="absolute z-50 mt-1.5 max-h-60 w-full overflow-auto rounded-xl border border-cream-300 bg-white py-1 text-sm shadow-lg dark:border-zinc-700 dark:bg-zinc-800">
          {options.map((opt) => (
            <li key={opt.value}>
              <button
                type="button"
                onClick={() => {
                  onChange(opt.value);
                  setOpen(false);
                }}
                className={`w-full cursor-pointer px-3.5 py-2 text-left transition hover:bg-cream-100 dark:hover:bg-zinc-700 ${
                  opt.value === value
                    ? "font-medium text-warm-900 dark:text-zinc-50"
                    : "text-warm-600 dark:text-zinc-300"
                }`}
              >
                {opt.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
