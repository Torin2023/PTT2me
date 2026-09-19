import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import { headers } from "next/headers";
import "./globals.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin", "cyrillic"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin", "cyrillic"],
});

export async function generateMetadata(): Promise<Metadata> {
  const requestHeaders = await headers();
  const host = requestHeaders.get("host") ?? "localhost";
  const protocol = host.startsWith("localhost") ? "http" : "https";
  const origin = `${protocol}://${host}`;

  return {
    metadataBase: new URL(origin),
    title: "PTT2me — локальная диктовка для macOS",
    description:
      "PTT2me 1.3.1: локальная диктовка на Apple Silicon. Подготовка активного приложения к вставке. Unsigned preview; совместимость с приложениями вручную не проверена.",
    openGraph: {
      type: "website",
      locale: "ru_RU",
      title: "PTT2me — локальная диктовка для macOS",
      description:
        "PTT2me 1.3.1: локальная диктовка на Apple Silicon. Подготовка активного приложения к вставке. Unsigned preview; совместимость с приложениями вручную не проверена.",
      images: [
        {
          url: "/og.png",
          width: 1200,
          height: 630,
          alt: "PTT2me — Говорите, текст уже там",
        },
      ],
    },
    twitter: {
      card: "summary_large_image",
      title: "PTT2me — локальная диктовка для macOS",
      description:
        "PTT2me 1.3.1: локальная диктовка на Apple Silicon. Подготовка активного приложения к вставке. Unsigned preview; совместимость с приложениями вручную не проверена.",
      images: ["/og.png"],
    },
    icons: {
      icon: "/favicon.svg",
      shortcut: "/favicon.svg",
    },
  };
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="ru">
      <body
        className={`${geistSans.variable} ${geistMono.variable} antialiased`}
      >
        {children}
      </body>
    </html>
  );
}
