# myUplink til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for NIBE-varmepumper og andre anlæg i myUplink.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-nibe-myuplink
```

og tryk **Installér**.

myUplink kræver, at du opretter din egen applikation:

1. Log ind på [dev.myuplink.com](https://dev.myuplink.com) med den konto, du bruger i myUplink-appen.
2. Opret en applikation under **Applications**. Som callback URL kan du skrive din HomewrightHQ-adresse med `/auth/external/callback` efter, f.eks. `http://localhost:8123/auth/external/callback`.
3. Gem applikationens Client Identifier og Client Secret.

Tilføj derefter **myUplink (NIBE)** under **Tilføj integration**, og skriv Client Identifier og Client Secret. Der er to måder at logge ind:

- **Client credentials** (standard): HomewrightHQ logger ind som applikationen og ser de anlæg, kontoen, der oprettede den, har adgang til. Der er ingen browser involveret.
- **Login i browseren**: næste trin viser en adresse til myUplinks login. Efter login sender myUplink browseren til callback-adressen. Den side kan vise en fejl eller "Linket er udløbet"; det gør ikke noget. Kopiér hele adressen fra adresselinjen (den indeholder `code=...`) og indsæt den i formularen. En ekstern integration kan ikke selv tage imod svaret fra `/auth/external/callback`.

## Hvad du får

Hvert anlæg bliver en enhed. Har systemet kun ét anlæg, får enheden systemets navn. Hvert datapunkt, myUplink oplyser for anlægget, bliver en entity med myUplinks navn for det:

- målinger bliver sensorer, med enhed og type ud fra enheden (°C, kW, kWh, A, Hz, bar, timer ...)
- målinger med navngivne værdier (f.eks. "Priority": Off, Hot water, Heating) bliver sensorer med teksten som tilstand
- indstillinger med et interval (f.eks. gradminutter, forskydning af varmekurven eller rumtemperatur) bliver tal, der kan ændres
- indstillinger med navngivne værdier (f.eks. varmtvandsbehov Small, Normal, Large) bliver valglister

Der er også en alarm-sensor, der er tændt, når systemet har en aktiv alarm. Dukker der nye datapunkter op, får de en entity ved næste opdatering. Navnene hentes på dansk med `Accept-Language: da`, hvor myUplink har en oversættelse.

Datapunkterne hentes hvert femte minut, og systemlisten (alarmer og forbindelse) hver halve time. Under **Indstillinger** på integrationen kan tiden sættes fra 1 til 60 minutter. Svarer myUplink 429, holder integrationen pause, så længe myUplink beder om, eller 15 minutter.

## myUplink Premium

At ændre indstillinger fra andre programmer kræver abonnementet myUplink Premium (Manage) på anlægget. Uden det svarer myUplink 403, kommandoen fejler, og HomewrightHQ viser et problem, der forklarer hvorfor. Læsning virker uden Premium.

## Intet klimaanlæg

Integrationen laver ikke et klimaanlæg. Hvilke datapunkter der er rumtemperatur, setpunkt og varme/køl, afhænger af model og antal klimasystemer, og myUplinks `smartHomeCategories` er ikke udfyldt på alle modeller. Setpunkter og forskydninger kan ændres som tal i stedet.

## Ikke afprøvet mod det rigtige API

Integrationen er skrevet ud fra myUplinks API-dokumentation og den måde, Home Assistants `myuplink`-integration bruger det på, og afprøvet mod en falsk myUplink med svar formet som de rigtige. Disse ting er ikke kontrolleret:

- At client credentials giver adgang til kontoens egne anlæg med scopet `READSYSTEM WRITESYSTEM`.
- At `minValue`, `maxValue` og `stepValue` skal ganges med `scaleValue`, mens `value` allerede er skaleret, og at en ændring sendes som den skalerede værdi (f.eks. `{"47398": 22.5}`).
- At valglister sendes som tal (`{"47041": 2}`).
- At myUplink svarer 403 på en ændring uden Premium.
- Om myUplink har en grænse for antal forespørgsler, og hvor den ligger. Fem minutter pr. anlæg er et forsigtigt valg.
- Om `Accept-Language: da` giver danske navne.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: OAuth 2.0 og myUplink-API'et
- `src/model.rs`: anlæg og datapunkter, og hvordan de bliver til entities
- `src/lib.rs`: opsætningen og forbindelsen til HomewrightHQ
- `tests/myuplink.rs`: tests mod en falsk myUplink med svarene i `tests/fixtures`, og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.1.0 && git push origin v0.1.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
