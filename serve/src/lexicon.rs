//! THE WORDS THE DETERMINISTIC LAYER READS, AS DATA (operator 2026-09-29: "the deterministic layer is completely agnostic, so it should work in any case").
//!
//! Nothing in `front.rs` is English any more. The parts of the layer that are agnostic by nature - the state of the conversation (a plan or questions waiting),
//! a chip matched by its exact text, a path on disk, a question mark in any script, a slash prefix, a symbol-only message - need no words. The parts that do -
//! "yes" and "cancel", an action verb, a question opener, a pleasantry - come from PACKS, one per language, all read at once, so a Portuguese "cancela" and an
//! English "cancel" are the same fact. A language that has no pack is not a failure: its messages are simply NOT DECIDED here, and the voice (a model, which reads
//! any language) writes the typed intent. A user adds a language, or a word, without touching code: `~/.antiloki/lexicon.json`
//! (`{"pl": {"yes": ["tak"], "no": ["nie", "anuluj"], "actions": ["dodaj"], ...}}`) is merged into the built-in packs.
use std::sync::OnceLock;

/// one language's words. Every list is matched on the message NORMALISED (lower-cased, punctuation removed, any script), as whole words or whole phrases.
#[derive(Default, Clone, Debug)]
pub struct Pack {
    pub yes: Vec<String>, pub no: Vec<String>, pub actions: Vec<String>, pub openers: Vec<String>, pub pleasantries: Vec<String>,
    pub polite: Vec<String>, pub status: Vec<String>, pub symptoms: Vec<String>, pub referents: Vec<String>, pub referring: Vec<String>, pub suggestion: Vec<String>, pub follow_ups: Vec<String>,
    pub removes: Vec<String>, pub reverts: Vec<String>, pub delegating: Vec<String>, pub creates: Vec<String>, pub relocates: Vec<String>, pub dangling: Vec<String>,
    /// words that make a request RISKY - production, credentials, billing, migrations, deploys - so the router goes up a tier on it (gauge::risk_ask)
    pub risky: Vec<String>,
}
fn v(s: &[&str]) -> Vec<String> { s.iter().map(|x| x.to_string()).collect() }

fn builtin() -> Vec<(&'static str, Pack)> {
    vec![
    ("en", Pack {
        yes: v(&["go", "go ahead", "yes", "y", "yep", "yeah", "ok", "okay", "sure", "do it", "proceed", "approve", "approved", "approve it", "run it", "start", "continue", "lets go", "let's go", "confirm", "sounds good", "looks good", "lgtm", "ship it", "go for it", "please do", "yes please", "yes go", "ok go", "okay go"]),
        no: v(&["cancel", "cancel it", "cancel that", "stop", "abort", "no", "nope", "never mind", "nevermind", "forget it", "dont", "don't", "don't do it", "dont do it", "do not", "no thanks", "drop it", "scrap it"]),
        actions: v(&["add", "fix", "make", "build", "create", "implement", "remove", "delete", "rename", "refactor", "change", "update", "write", "move", "replace", "split", "extract", "install", "convert", "migrate", "optimize", "optimise", "improve", "clean", "wire", "enable", "disable", "style", "document", "test", "redesign", "restyle", "debug", "upgrade", "revert", "port", "generate", "deploy", "merge", "set up"]),
        openers: v(&["in which", "in what", "what", "why", "how", "where", "when", "which", "who", "whose", "explain", "describe", "show me", "list", "tell me", "is there", "is it", "are there", "does", "do you", "did", "walk me through", "summarize", "summarise", "remind me", "what's", "whats", "compare", "find out", "give me an overview"]),
        pleasantries: v(&["thanks", "thank you", "thank you so much", "thanks a lot", "great thanks", "great, thanks", "hello", "hi", "hey", "hi there", "hello there", "ok cool", "cool", "nice", "nice work", "great", "good morning", "good night", "good evening", "lol", "haha", "got it", "perfect", "awesome", "hmm", "nope", "ok", "okay", "who are you", "what are you"]),
        polite: v(&["can you", "could you", "would you", "will you", "can we", "could we", "we should", "we need to", "i want to", "i need to", "i would like to", "i'd like to", "lets", "let's", "please"]),
        status: v(&["how's it going", "how is it going", "hows it going", "is it done", "any progress", "still working", "what's the status", "whats the status", "how is the run", "how's the run", "status of the run", "are you done"]),
        symptoms: v(&["is broken", "are broken", "crashes", "crashing", "is failing", "are failing", "fails", "doesn't work", "does not work", "not working", "look wrong", "looks wrong", "is wrong", "throws", "throwing", "regression"]),
        referents: v(&["that", "those", "it", "them", "the first", "the second", "the third", "the one", "this one", "the same"]),
        referring: v(&["go", "yes", "ok", "okay", "do", "and", "apply", "sure", "proceed", "continue", "now", "then"]),
        suggestion: v(&["how about", "what about", "why not", "why don't we", "why dont we", "what if we"]),
        follow_ups: v(&["go","yes","ok","okay","do","implement","apply","that","those","it","the","also","and","now","then","proceed","continue","start","please","lets","let's"]),
        removes: v(&["remove","delete","drop","get rid of","take out","strip","clean up","clear","erase","cut","prune","uninstall","deprecate","disable"]), reverts: v(&["revert","undo","discard","roll back","rollback","restore","reset","wipe","start over","put back"]),
        delegating: v(&["tell me what to","whatever you think is best","what you think is best","in my place","in my shoes", "tell me what's best","what's best","thoughts on what","what to improve","what could be improved","propose something","recommend something","your thoughts","anything you'd improve","what is missing","what's missing", "what would you build","what would you do next","you pick","decide for me","your take","what to do next","what to work on","what to build next", "what do you suggest","what would you suggest","what would you do","what should i do","what should we do","what do you recommend","what would you recommend","any suggestions","any ideas","any recommendations","your suggestions","your recommendation","your opinion","what do you think","suggest something","suggest improvements","what next","what's next","what is next","what would you improve","what should we work on","surprise me","you choose","your call","you decide","up to you","pick something","your pick"]), creates: v(&["create","scaffold","generate","port","extract","add a new","add new","new file","write a new","make a new"]), relocates: v(&["copy","duplicate","cp","clone", "rename","move","relocate","mv"]), dangling: v(&["what we did before","same again","repeat what you did","do the same","as previously","what you did", "you did yesterday","the same thing","did last time","as you did","like you did", "as last time","same fix as","the same as last time","same thing as before","same as previously", "the same way","like that one","like before","as we did","do it again","same as the other","like the other one","same as before","like last time","as before"]),
        risky: v(&["production","prod","credentials","password","passwords","secret","secrets","api key","private key","billing","payment","payments","migration","migrations","deploy","drop table","customer data"]),
    }),
    ("pt", Pack {
        yes: v(&["sim", "claro", "pode ir", "pode fazer", "pode", "faz", "vai", "vai lá", "vai la", "vamos", "bora", "ok", "tá bom", "ta bom", "aprovado", "aprovo", "manda ver", "manda", "pode ser", "isso", "isso mesmo", "perfeito", "vai em frente", "com certeza"]),
        no: v(&["não", "nao", "cancela", "cancelar", "cancele", "para", "pare", "parar", "deixa pra lá", "deixa pra la", "esquece", "nem pensar", "melhor não", "melhor nao", "não faça", "nao faca", "abortar", "não precisa", "nao precisa"]),
        actions: v(&["adicione", "adicionar", "adiciona", "corrija", "corrigir", "corrige", "crie", "criar", "cria", "remova", "remover", "remove", "exclua", "excluir", "apague", "apagar", "renomeie", "renomear", "atualize", "atualizar", "implemente", "implementar", "mude", "mudar", "altere", "alterar", "melhore", "melhorar", "refatore", "refatorar", "escreva", "escrever", "mova", "mover", "substitua", "substituir", "divida", "dividir", "extraia", "extrair", "instale", "instalar", "converta", "converter", "migre", "migrar", "otimize", "otimizar", "limpe", "limpar", "habilite", "desabilite", "estilize", "documente", "documentar", "teste", "testar", "depure", "depurar", "faça", "faca", "fazer", "construa", "construir", "gere", "gerar", "configure", "configurar", "ajuste", "ajustar", "coloque", "colocar", "torne", "deixe", "deixar"]),
        openers: v(&["onde", "por que", "por quê", "porque", "como", "o que", "oque", "qual", "quais", "quem", "quando", "quanto", "quantos", "explique", "explica", "mostre", "mostra", "liste", "lista", "me diga", "me explica", "existe", "há", "tem", "em qual", "em que", "você sabe", "voce sabe", "me lembra"]),
        pleasantries: v(&["obrigado", "obrigada", "valeu", "muito obrigado", "muito obrigada", "oi", "olá", "ola", "bom dia", "boa tarde", "boa noite", "tchau", "legal", "show", "beleza", "entendi", "ótimo", "otimo", "blz", "haha", "kkk", "kkkk", "quem é você", "quem e voce"]),
        polite: v(&["você pode", "voce pode", "pode", "poderia", "podemos", "dá pra", "da pra", "por favor", "quero", "preciso", "precisamos", "vamos", "gostaria de", "temos que", "precisa"]),
        status: v(&["como está indo", "como esta indo", "como vai indo", "já terminou", "ja terminou", "já acabou", "ja acabou", "está pronto", "esta pronto", "ficou pronto", "qual o status", "algum progresso", "ainda está rodando", "ainda esta rodando"]),
        symptoms: v(&["está quebrado", "esta quebrado", "quebrou", "não funciona", "nao funciona", "dá erro", "da erro", "está falhando", "esta falhando", "falha", "travando", "crashando", "está errado", "esta errado", "parece errado"]),
        referents: v(&["isso", "esse", "essa", "esses", "essas", "aquele", "aquilo", "o primeiro", "a primeira", "o segundo", "a segunda", "o mesmo"]),
        referring: v(&["vai", "faz", "faça", "faca", "sim", "isso", "e", "agora", "então", "entao", "manda", "pode", "ok", "vamos", "aplique"]),
        suggestion: v(&["que tal", "e se", "por que não", "porque não", "e que tal"]),
        follow_ups: v(&["vai","sim","ok","faz","faça","faca","implemente","aplique","isso","esse","essa","aquele","o","a","também","tambem","e","agora","então","entao","continue","pode","por favor","vamos"]),
        removes: v(&["remova","remover","exclua","excluir","apague","apagar","tire","tirar","limpe","limpar","elimine","eliminar","desinstale","desabilite","corte","cortar"]), reverts: v(&["reverta","reverter","desfaça","desfaca","desfazer","descarte","descartar","restaure","restaurar","volte","voltar","recomece"]),
        delegating: v(&["o que você faria no meu lugar","o que voce faria no meu lugar","qual sua sugestão","qual sua sugestao","próximo passo","proximo passo", "sugira algo","sugira uma","sugira melhorias","me sugira","o que falta","o que você melhoraria","o que voce melhoraria","o que seria melhor","diga você","diga voce", "qual é a sua sugestão","qual e a sua sugestao","qual a sua sugestão","qual a sua opinião","decida por mim","você escolhe","o que construir", "o que você sugere","o que voce sugere","o que você recomenda","o que voce recomenda","o que você faria","o que voce faria","o que devo fazer","o que devemos fazer","alguma sugestão","alguma sugestao","alguma ideia","me dê sugestões","me de sugestoes","sua sugestão","sua opinião","sua opiniao","o que você acha","o que voce acha","você escolhe","voce escolhe","você decide","voce decide","a escolha é sua","a escolha e sua","o que melhorar","o que fazer a seguir","me surpreenda"]), creates: v(&["crie","criar","cria","gere","gerar","extraia","extrair","novo arquivo","um novo","uma nova","escreva um novo","escreva uma nova"]), relocates: v(&["copie","copiar","duplique","duplicar","clone", "renomeie","renomear","mova","mover","transfira","transferir"]), dangling: v(&["o que fizemos antes","de novo igual","repita o que","faça o mesmo","faca o mesmo", "igual à última vez","igual a ultima vez","como você fez","como voce fez","o mesmo de ontem", "como da vez passada","igual à vez passada","igual a vez passada","como sempre", "do mesmo jeito","da mesma forma","como antes","faça de novo","faca de novo","faz de novo","como da última vez","como da ultima vez","igual ao outro","igual à outra","igual a outra","como fizemos"]),
        risky: v(&["produção","producao","credenciais","senha","senhas","segredo","segredos","chave de api","chave privada","cobrança","cobranca","pagamento","pagamentos","migração","migracao","deploy","dados de clientes","dados dos clientes"]),
    }),
    ("es", Pack {
        yes: v(&["sí", "si", "claro", "adelante", "dale", "vale", "de acuerdo", "hazlo", "aprobado", "procede", "sigue", "ok", "perfecto", "por supuesto", "hazlo ya"]),
        no: v(&["no", "cancela", "cancelar", "para", "detente", "olvídalo", "olvidalo", "déjalo", "dejalo", "mejor no", "no lo hagas", "aborta"]),
        actions: v(&["añade", "añadir", "agrega", "agregar", "arregla", "arreglar", "corrige", "corregir", "crea", "crear", "elimina", "eliminar", "borra", "borrar", "renombra", "renombrar", "actualiza", "actualizar", "implementa", "implementar", "cambia", "cambiar", "mejora", "mejorar", "refactoriza", "refactorizar", "escribe", "escribir", "mueve", "mover", "reemplaza", "reemplazar", "divide", "dividir", "extrae", "extraer", "instala", "instalar", "convierte", "convertir", "migra", "migrar", "optimiza", "optimizar", "limpia", "limpiar", "habilita", "deshabilita", "documenta", "documentar", "prueba", "probar", "depura", "depurar", "haz", "hacer", "construye", "construir", "genera", "generar", "configura", "configurar", "ajusta", "ajustar", "pon", "poner", "deja", "dejar"]),
        openers: v(&["dónde", "donde", "por qué", "porque", "cómo", "como", "qué", "cuál", "cual", "cuáles", "cuales", "quién", "quien", "cuándo", "cuando", "cuánto", "cuanto", "cuántos", "explica", "explícame", "muestra", "muéstrame", "lista", "dime", "existe", "hay", "en qué", "en cuál"]),
        pleasantries: v(&["gracias", "muchas gracias", "hola", "buenos días", "buenos dias", "buenas tardes", "buenas noches", "adiós", "adios", "genial", "entendido", "perfecto", "vale", "jaja", "quién eres", "quien eres"]),
        polite: v(&["puedes", "podrías", "podrias", "podemos", "por favor", "quiero", "necesito", "necesitamos", "vamos a", "me gustaría", "me gustaria", "deberíamos", "deberiamos"]),
        status: v(&["cómo va la ejecución", "como va la ejecucion", "cómo va eso", "como va eso", "ya terminó", "ya termino", "está listo", "esta listo", "cuál es el estado", "cual es el estado", "algún progreso", "algun progreso", "sigue corriendo"]),
        symptoms: v(&["está roto", "esta roto", "se rompió", "se rompio", "no funciona", "da error", "falla", "fallando", "se cae", "está mal", "esta mal"]),
        referents: v(&["eso", "esto", "ese", "esa", "esos", "esas", "el primero", "la primera", "el segundo", "la segunda", "el mismo"]),
        referring: v(&["sí", "si", "hazlo", "y", "ahora", "entonces", "dale", "vale", "ok", "aplica", "procede"]),
        suggestion: v(&["qué tal", "que tal", "y si", "por qué no", "por que no"]),
        follow_ups: v(&["sí","si","ok","hazlo","implementa","aplica","eso","esto","ese","esa","el","la","también","tambien","y","ahora","entonces","continúa","continua","por favor","vamos","dale"]),
        removes: v(&["elimina","eliminar","borra","borrar","quita","quitar","saca","limpia","limpiar","desinstala","deshabilita","corta","cortar"]), reverts: v(&["revierte","revertir","deshaz","deshacer","descarta","descartar","restaura","restaurar","vuelve","volver","empieza de nuevo"]),
        delegating: v(&["qué me recomiendas","que me recomiendas","qué me sugieres","que me sugieres", "dime tú","dime tu","qué hacer","que hacer","qué falta","que falta","propón algo","propon algo","sugiere algo", "qué mejorarías","que mejorarias","decide por mí","decide por mi","tú escoges","tu escoges", "qué sugieres","que sugieres","qué recomiendas","que recomiendas","qué harías","que harias","qué debo hacer","que debo hacer","alguna sugerencia","alguna idea","dame sugerencias","tu opinión","tu opinion","qué opinas","que opinas","tú eliges","tu eliges","tú decides","tu decides","a tu elección","a tu eleccion","sorpréndeme","sorprendeme","qué sigue","que sigue"]), creates: v(&["crea","crear","genera","generar","extrae","extraer","nuevo archivo","un nuevo","una nueva"]), relocates: v(&["copia","copiar","duplica","duplicar", "renombra","renombrar","mueve","mover"]), dangling: v(&["lo que hicimos antes","otra vez igual","repite lo que","haz lo mismo", "igual que antes","como hiciste","lo mismo de ayer","lo mismo que la última vez", "como la vez pasada","igual que la vez pasada","como siempre", "como la vez anterior","como antes","de la misma manera","de la misma forma","hazlo otra vez","como la última vez","como la ultima vez","igual que el otro","como hicimos"]),
        risky: v(&["producción","produccion","credenciales","contraseña","contrasena","contraseñas","secreto","secretos","clave de api","clave privada","facturación","facturacion","pago","pagos","migración","migracion","despliegue","datos de clientes"]),
    }),
    ("fr", Pack {
        yes: v(&["oui", "ouais", "d'accord", "vas-y", "vas y", "ok", "parfait", "approuvé", "approuve", "fais-le", "fais le", "c'est bon", "bien sûr", "bien sur", "allez"]),
        no: v(&["non", "annule", "annuler", "arrête", "arrete", "stop", "laisse tomber", "oublie", "pas maintenant", "ne le fais pas", "surtout pas"]),
        actions: v(&["ajoute", "ajouter", "corrige", "corriger", "répare", "repare", "réparer", "reparer", "crée", "cree", "créer", "creer", "supprime", "supprimer", "retire", "retirer", "renomme", "renommer", "mets à jour", "mettre à jour", "implémente", "implemente", "implémenter", "change", "changer", "modifie", "modifier", "améliore", "ameliore", "améliorer", "refactorise", "refactoriser", "écris", "ecris", "écrire", "déplace", "deplace", "déplacer", "remplace", "remplacer", "sépare", "separe", "extrais", "extraire", "installe", "installer", "convertis", "convertir", "migre", "migrer", "optimise", "optimiser", "nettoie", "nettoyer", "active", "désactive", "desactive", "documente", "documenter", "teste", "tester", "déboguer", "fais", "faire", "construis", "construire", "génère", "genere", "générer", "configure", "configurer", "ajuste", "ajuster"]),
        openers: v(&["où", "ou est", "pourquoi", "comment", "qu'est-ce que", "quel", "quelle", "quels", "quelles", "qui", "quand", "combien", "explique", "montre", "liste", "dis-moi", "dis moi", "y a-t-il", "y a t il", "est-ce que", "dans quel", "dans quelle"]),
        pleasantries: v(&["merci", "merci beaucoup", "salut", "bonjour", "bonsoir", "au revoir", "super", "génial", "genial", "compris", "parfait", "haha", "qui es-tu", "qui es tu"]),
        polite: v(&["peux-tu", "peux tu", "tu peux", "pouvez-vous", "pouvez vous", "vous pouvez", "s'il te plaît", "s'il te plait", "s'il vous plaît", "s'il vous plait", "je veux", "j'ai besoin de", "on doit", "il faut", "on devrait"]),
        status: v(&["ça avance", "ca avance", "c'est fini", "c'est prêt", "c'est pret", "où en est", "ou en est", "quel est le statut", "un progrès", "un progres"]),
        symptoms: v(&["ne marche pas", "ne fonctionne pas", "est cassé", "est casse", "plante", "erreur", "ça plante", "ca plante", "est faux"]),
        referents: v(&["ça", "ca", "cela", "ce", "cette", "ces", "le premier", "la première", "la premiere", "le second", "le même", "le meme"]),
        referring: v(&["oui", "vas-y", "vas y", "et", "maintenant", "alors", "fais", "ok", "applique"]),
        suggestion: v(&["et si", "pourquoi pas", "que dirais-tu", "et pourquoi pas"]),
        follow_ups: v(&["oui","ok","fais","implémente","implemente","applique","ça","ca","cela","ce","le","la","aussi","et","maintenant","alors","continue","s'il te plaît","vas-y","allons"]),
        removes: v(&["supprime","supprimer","retire","retirer","enlève","enleve","enlever","efface","effacer","nettoie","nettoyer","désinstalle","desinstalle","désactive","desactive"]), reverts: v(&["annule","annuler","défais","defais","défaire","defaire","rétablis","retablis","restaure","restaurer","reviens","recommence"]),
        delegating: v(&["à ma place","a ma place","que dois-je faire","que dois je faire", "dis-moi ce qui","dis moi ce qui","qu'est-ce qui manque","propose quelque chose","tu proposes quoi", "décide pour moi","decide pour moi","à toi de décider","a toi de decider", "que me conseilles-tu","que me conseilles tu","que suggères-tu","que suggeres tu","que proposes-tu","que proposes tu","que recommandes-tu","que recommandes tu","que ferais-tu","que ferais tu","des idées","des idees","une suggestion","ton avis","qu'en penses-tu","qu'en penses tu","tu choisis","à toi de choisir","a toi de choisir","surprends-moi","surprends moi","et ensuite"]), creates: v(&["crée","cree","créer","creer","génère","genere","générer","generer","extrais","extraire","nouveau fichier","un nouveau","une nouvelle"]), relocates: v(&["copie","copier","duplique","dupliquer", "renomme","renommer","déplace","deplace","déplacer","deplacer"]), dangling: v(&["ce qu'on a fait avant","refais pareil","fais la même chose","fais la meme chose", "comme hier","comme tu as fait","la même chose","la meme chose", "la fois d'avant","comme la fois","comme d'habitude","pareil qu'avant", "comme avant","comme la dernière fois","comme la derniere fois","de la même façon","de la meme facon","refais-le","refais le","comme l'autre","comme on a fait"]),
        risky: v(&["production","identifiants","mot de passe","mots de passe","secret","secrets","clé api","cle api","clé privée","cle privee","facturation","paiement","paiements","migration","déploiement","deploiement","données clients","donnees clients"]),
    }),
    ("de", Pack {
        yes: v(&["ja", "jawohl", "klar", "los", "mach das", "mach es", "okay", "ok", "genehmigt", "weiter", "einverstanden", "in ordnung", "gerne"]),
        no: v(&["nein", "abbrechen", "abbrechen bitte", "stopp", "stop", "lass es", "vergiss es", "lieber nicht", "nicht machen"]),
        actions: v(&["füge", "fuege", "ergänze", "ergaenze", "ergänzen", "behebe", "beheben", "repariere", "reparieren", "erstelle", "erstellen", "lösche", "loesche", "löschen", "entferne", "entfernen", "benenne", "aktualisiere", "aktualisieren", "implementiere", "implementieren", "ändere", "aendere", "ändern", "verbessere", "verbessern", "refaktoriere", "schreibe", "schreiben", "verschiebe", "ersetze", "ersetzen", "teile", "extrahiere", "installiere", "konvertiere", "migriere", "optimiere", "bereinige", "aktiviere", "deaktiviere", "dokumentiere", "teste", "testen", "debugge", "mache", "mach", "baue", "bauen", "generiere", "konfiguriere", "passe"]),
        openers: v(&["wo", "warum", "wieso", "weshalb", "wie", "was", "welche", "welcher", "welches", "wer", "wann", "wieviel", "wie viele", "erkläre", "erklaere", "zeige", "zeig", "liste", "sag mir", "gibt es", "in welcher", "in welchem"]),
        pleasantries: v(&["danke", "vielen dank", "hallo", "guten morgen", "guten tag", "guten abend", "tschüss", "tschuess", "super", "verstanden", "prima", "haha", "wer bist du"]),
        polite: v(&["kannst du", "könntest du", "koenntest du", "können wir", "koennen wir", "bitte", "ich möchte", "ich moechte", "ich will", "wir sollten", "wir müssen", "wir muessen"]),
        status: v(&["wie läuft es", "wie laeuft es", "ist es fertig", "schon fertig", "wie ist der stand", "fortschritt"]),
        symptoms: v(&["funktioniert nicht", "ist kaputt", "stürzt ab", "stuerzt ab", "fehler", "geht nicht", "ist falsch"]),
        referents: v(&["das", "dies", "diese", "den ersten", "die erste", "den zweiten", "dasselbe"]),
        referring: v(&["ja", "mach", "und", "jetzt", "dann", "ok", "okay", "wende"]),
        suggestion: v(&["wie wäre es", "wie waere es", "was ist mit", "warum nicht"]),
        follow_ups: v(&["ja","ok","okay","mach","implementiere","wende","das","dies","die","der","auch","und","jetzt","dann","weiter","bitte","los"]),
        removes: v(&["entferne","entfernen","lösche","loesche","löschen","loeschen","streiche","räume auf","raeume auf","deinstalliere","deaktiviere"]), reverts: v(&["setze zurück","setze zurueck","mache rückgängig","mache rueckgaengig","verwirf","stelle wieder her","zurücksetzen","zuruecksetzen"]),
        delegating: v(&["was sollten wir","was soll ich","was würdest du an meiner stelle","was wuerdest du an meiner stelle", "was meinst du","was fehlt","was würdest du verbessern","was wuerdest du verbessern","schlag etwas vor","sag du", "entscheide für mich","entscheide fuer mich","du suchst aus", "was schlägst du vor","was schlaegst du vor","was empfiehlst du","was würdest du tun","was wuerdest du tun","was soll ich tun","was sollen wir tun","irgendwelche vorschläge","irgendwelche vorschlaege","irgendwelche ideen","deine meinung","was denkst du","du entscheidest","du wählst","du waehlst","überrasch mich","ueberrasch mich","was als nächstes","was als naechstes"]), creates: v(&["erstelle","erstellen","generiere","generieren","extrahiere","extrahieren","neue datei","eine neue","ein neues"]), relocates: v(&["kopiere","kopieren","dupliziere","duplizieren", "benenne","umbenennen","verschiebe","verschieben"]), dangling: v(&["was wir vorher gemacht haben","nochmal dasselbe","wiederhole was","mach dasselbe", "wie gestern","wie du es gemacht hast","dasselbe wie", "wie das letzte mal","wie immer","genauso wie vorher", "wie beim letzten mal","wie vorher","wie zuvor","auf die gleiche weise","mach es noch einmal","wie das andere","wie wir es gemacht haben"]),
        risky: v(&["produktion","produktivsystem","zugangsdaten","passwort","passwörter","passwoerter","geheimnis","geheimnisse","private key","rechnungsstellung","abrechnung","zahlung","zahlungen","migration","deployment","kundendaten"]),
    }),
    ("it", Pack {
        yes: v(&["sì", "si", "certo", "vai", "procedi", "va bene", "ok", "approvato", "fallo", "perfetto", "d'accordo"]),
        no: v(&["no", "annulla", "annullare", "ferma", "fermati", "lascia stare", "dimentica", "meglio di no", "non farlo"]),
        actions: v(&["aggiungi", "aggiungere", "correggi", "correggere", "sistema", "sistemare", "crea", "creare", "elimina", "eliminare", "rimuovi", "rimuovere", "rinomina", "rinominare", "aggiorna", "aggiornare", "implementa", "implementare", "cambia", "cambiare", "modifica", "modificare", "migliora", "migliorare", "rifattorizza", "scrivi", "scrivere", "sposta", "spostare", "sostituisci", "sostituire", "dividi", "estrai", "estrarre", "installa", "installare", "converti", "convertire", "migra", "migrare", "ottimizza", "ottimizzare", "pulisci", "pulire", "abilita", "disabilita", "documenta", "testa", "testare", "debugga", "fai", "fare", "costruisci", "genera", "generare", "configura", "configurare"]),
        openers: v(&["dove", "perché", "perche", "come", "cosa", "che cosa", "quale", "quali", "chi", "quando", "quanto", "quanti", "spiega", "mostra", "elenca", "dimmi", "esiste", "c'è", "ce", "in quale"]),
        pleasantries: v(&["grazie", "mille grazie", "ciao", "buongiorno", "buonasera", "arrivederci", "perfetto", "capito", "ottimo", "haha", "chi sei"]),
        polite: v(&["puoi", "potresti", "possiamo", "per favore", "voglio", "ho bisogno di", "dobbiamo", "vorrei"]),
        status: v(&["hai finito", "è pronto", "e pronto", "a che punto", "stato", "progressi"]),
        symptoms: v(&["non funziona", "è rotto", "e rotto", "si blocca", "errore", "è sbagliato"]),
        referents: v(&["quello", "questo", "quella", "il primo", "la prima", "il secondo", "lo stesso"]),
        referring: v(&["sì", "si", "fallo", "e", "ora", "poi", "vai", "ok", "applica"]),
        suggestion: v(&["che ne dici", "e se", "perché no", "perche no"]),
        follow_ups: v(&["sì","si","ok","fallo","implementa","applica","quello","questo","il","la","anche","e","ora","poi","continua","per favore","vai"]),
        removes: v(&["rimuovi","rimuovere","elimina","eliminare","cancella","cancellare","togli","togliere","pulisci","pulire","disinstalla","disabilita"]), reverts: v(&["ripristina","ripristinare","annulla","annullare","scarta","scartare","torna indietro","ricomincia"]),
        delegating: v(&["al mio posto","cosa dovrei fare", "cosa manca","proponi qualcosa","dimmi tu","che cosa miglioreresti", "decidi per me","scegli tu per me", "cosa suggerisci","che cosa suggerisci","cosa consigli","che cosa consigli","cosa faresti","cosa devo fare","cosa dovremmo fare","qualche suggerimento","qualche idea","la tua opinione","cosa ne pensi","scegli tu","decidi tu","sorprendimi","cosa c'è dopo","cosa c e dopo"]), creates: v(&["crea","creare","genera","generare","estrai","estrarre","nuovo file","un nuovo","una nuova"]), relocates: v(&["copia","copiare","duplica","duplicare", "rinomina","rinominare","sposta","spostare"]), dangling: v(&["quello che abbiamo fatto prima","ancora lo stesso","ripeti quello che","fai lo stesso", "come ieri","come hai fatto","lo stesso di prima", "come l'altra volta","come sempre", "come prima","come l'ultima volta","allo stesso modo","fallo di nuovo","come l'altro","come abbiamo fatto"]),
        risky: v(&["produzione","credenziali","password","segreto","segreti","chiave api","chiave privata","fatturazione","pagamento","pagamenti","migrazione","deploy","dati dei clienti"]),
    }),
    ]
}

/// the union of every pack (built-in + `~/.antiloki/lexicon.json`), read once
pub fn all() -> &'static Pack {
    static ALL: OnceLock<Pack> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut u = Pack::default();
        let mut add = |p: &Pack| {
            macro_rules! ext { ($f:ident) => { for w in &p.$f { let w = w.trim().to_lowercase(); if !w.is_empty() && !u.$f.contains(&w) { u.$f.push(w); } } } }
            ext!(yes); ext!(no); ext!(actions); ext!(openers); ext!(pleasantries); ext!(polite); ext!(status); ext!(symptoms); ext!(referents); ext!(referring); ext!(suggestion); ext!(follow_ups); ext!(removes); ext!(reverts); ext!(delegating); ext!(creates); ext!(relocates); ext!(dangling); ext!(risky);
        };
        for (_, p) in builtin() { add(&p); }
        for p in user_packs() { add(&p); }
        // longest phrases first, so "how is it going" is tried before "how"
        for l in [&mut u.polite, &mut u.openers] { l.sort_by_key(|w| std::cmp::Reverse(w.chars().count())); }
        u
    })
}

/// `~/.antiloki/lexicon.json`: `{ "<lang>": { "yes": [...], "no": [...], "actions": [...], ... } }`. A missing or malformed file is no file.
fn user_packs() -> Vec<Pack> {
    let path = std::env::var("ANTILOKI_LEXICON").ok().map(std::path::PathBuf::from).or_else(|| std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".antiloki").join("lexicon.json")));
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return vec![] };
    let Ok(serde_json::Value::Object(langs)) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let list = |o: &serde_json::Value, k: &str| -> Vec<String> { o.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|w| w.as_str().map(String::from)).collect()).unwrap_or_default() };
    langs.values().map(|o| Pack { yes: list(o, "yes"), no: list(o, "no"), actions: list(o, "actions"), openers: list(o, "openers"), pleasantries: list(o, "pleasantries"), polite: list(o, "polite"),
        status: list(o, "status"), symptoms: list(o, "symptoms"), referents: list(o, "referents"), referring: list(o, "referring"), suggestion: list(o, "suggestion"), follow_ups: list(o, "follow_ups"), removes: list(o, "removes"), reverts: list(o, "reverts"), delegating: list(o, "delegating"), creates: list(o, "creates"), relocates: list(o, "relocates"), dangling: list(o, "dangling"), risky: list(o, "risky") }).collect()
}

/// Lower-case, every apostrophe one apostrophe, punctuation and symbols gone, ANY script kept (a chip in Japanese normalises to itself, not to nothing). Pure - tested.
pub fn norm(text: &str) -> String {
    text.to_lowercase().replace('’', "'").replace('‘', "'")
        .split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(|w| w.trim_matches('\'')).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// every mark that makes a sentence a question, in the scripts that have one
pub const QUESTION_MARKS: [char; 5] = ['?', '¿', '？', '؟', '՞'];

/// a word or phrase that opens with a preposition of place ("in src/cart.ts, change ..."): the target comes before the verb
pub const TARGET_PREPOSITIONS: &[&str] = &["in", "on", "for", "at", "inside", "within", "em", "no", "na", "nos", "nas", "para", "dentro", "en", "dentro de", "dans", "sur", "pour", "chez", "im", "auf", "für", "fuer", "bei", "su", "per", "nel", "nella"];
/// what the chat itself writes when it proposes work and waits for a go (its prompt says "say go"; the model may answer in the operator's language)
pub const PROPOSAL_CUES: &[&str] = &["say go", "say \"go\"", "tell antiloki go", "antiloki go", "reply go", "type go", "just say go", "say **go**", "go:", "diga go", "diga \"go\"", "responda go", "escreva go", "di go", "dis go", "sag go", "scrivi go", "manda go"];
const YES_SYMBOLS: &[char] = &['👍', '✅', '👌', '🆗', '✔', '☑', '🙆', '🤝', '🚀'];
const NO_SYMBOLS: &[char] = &['👎', '❌', '🛑', '⛔', '✖', '🚫', '🙅'];
/// an emoji reply that is a vote: 👍 approves, 🛑 cancels. Skin tones and variation selectors are ignored. `None` for anything else. Pure - tested.
pub fn symbol_vote(text: &str) -> Option<bool> {
    let cs: Vec<char> = text.chars().filter(|c| !c.is_whitespace() && *c != '\u{FE0F}' && *c != '\u{200D}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(c)).collect();
    if cs.is_empty() { return None }
    if cs.iter().all(|c| YES_SYMBOLS.contains(c)) { Some(true) } else if cs.iter().all(|c| NO_SYMBOLS.contains(c)) { Some(false) } else { None }
}
/// scripts written without spaces between words: their words cannot be found by splitting, only by prefix or substring
pub fn is_unspaced(s: &str) -> bool { s.chars().any(|c| matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF | 0x0E00..=0x0E7F | 0x0E80..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF)) }
/// `w` opens `n` as a whole word (or, in an unspaced script, as a prefix)
pub fn starts_with_word(n: &str, w: &str) -> bool { n == w || n.strip_prefix(w).map(|rest| rest.starts_with(' ') || is_unspaced(w)).unwrap_or(false) }
/// `phrase` occurs in `n` as whole words (or as a substring, in an unspaced script)
pub fn has_phrase(n: &str, phrase: &str) -> bool { if is_unspaced(phrase) { n.contains(phrase) } else { format!(" {n} ").contains(&format!(" {phrase} ")) } }

/// `phrase` occurs in `raw` (lower-cased, punctuation kept) on word edges where an identifier character - a letter, digit, `.`, `-`, `_`, `/` - does NOT touch it:
/// "what next" is in "what next?" and not in "explain what next.js does"; "surprise me" is not in "surprise-me button". Unspaced scripts match anywhere.
pub fn has_phrase_raw(raw: &str, phrase: &str) -> bool {
    if is_unspaced(phrase) { return raw.contains(phrase) }
    let ident = |c: char| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '/');
    let (rc, pc): (Vec<char>, Vec<char>) = (raw.chars().collect(), phrase.chars().collect());
    if pc.is_empty() || rc.len() < pc.len() { return false }
    (0..=rc.len() - pc.len()).any(|i| rc[i..i + pc.len()] == pc[..] && (i == 0 || !ident(rc[i - 1])) && rc.get(i + pc.len()).map(|c| !ident(*c) || (*c == '.' && rc.get(i + pc.len() + 1).map(|n| !n.is_alphanumeric()).unwrap_or(true))).unwrap_or(true))
}
/// does the ask, in any pack's language, REMOVE something / UNDO something? Whole-word / phrase match on the normalised text. Pure.
pub fn asks_to_remove(text: &str) -> bool { let n = norm(text); all().removes.iter().any(|k| has_phrase(&n, k)) }
pub fn asks_to_revert(text: &str) -> bool { let n = norm(text); all().reverts.iter().any(|k| has_phrase(&n, k)) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalisation_keeps_every_script_and_drops_only_punctuation() {
        assert_eq!(norm("  Não!! "), "não"); assert_eq!(norm("Ça va?"), "ça va"); assert_eq!(norm("日本語で追加して"), "日本語で追加して"); assert_eq!(norm("Да, пожалуйста"), "да пожалуйста");
        assert_eq!(norm("Don’t"), "don't"); assert_eq!(norm("👍"), ""); assert_eq!(norm("Ünïcödé Ärger"), "ünïcödé ärger");
    }
    #[test]
    fn symbols_vote_and_phrases_match_on_word_edges() {
        assert_eq!(symbol_vote("👍"), Some(true)); assert_eq!(symbol_vote("👍🏽 "), Some(true)); assert_eq!(symbol_vote("✅✅"), Some(true)); assert_eq!(symbol_vote("🛑"), Some(false));
        assert_eq!(symbol_vote("👍👎"), None); assert_eq!(symbol_vote("ok"), None); assert_eq!(symbol_vote(""), None);
        assert!(starts_with_word("add filters", "add") && !starts_with_word("address book", "add") && starts_with_word("追加してください", "追加") && starts_with_word("go", "go"));
        assert!(has_phrase("the cart crashes now", "crashes") && !has_phrase("the cart crashesx", "crashes") && has_phrase("カートが壊れています", "壊れ"));
    }
    #[test]
    fn every_pack_speaks_every_list_and_the_union_has_them_all() {
        for (lang, p) in builtin() {
            for (name, l) in [("yes", &p.yes), ("no", &p.no), ("actions", &p.actions), ("openers", &p.openers), ("pleasantries", &p.pleasantries), ("polite", &p.polite), ("status", &p.status), ("symptoms", &p.symptoms), ("referents", &p.referents), ("referring", &p.referring), ("suggestion", &p.suggestion), ("follow_ups", &p.follow_ups), ("removes", &p.removes), ("reverts", &p.reverts), ("delegating", &p.delegating), ("creates", &p.creates), ("relocates", &p.relocates), ("risky", &p.risky), ("dangling", &p.dangling), ("risky", &p.risky)] {
                assert!(!l.is_empty(), "{lang}.{name} is empty");
                for w in l.iter() { assert_eq!(w, &w.to_lowercase(), "{lang}.{name}: {w:?} must be lower-case"); assert!(!w.trim().is_empty()); }
            }
            // a "yes" is never also a "no" inside one language
            for w in &p.yes { assert!(!p.no.contains(w), "{lang}: {w:?} is both yes and no"); }
        }
        let a = all(); assert!(a.yes.contains(&"sim".to_string()) && a.no.contains(&"cancel".to_string()) && a.actions.contains(&"adicione".to_string()));
    }
}
