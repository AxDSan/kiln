/* Core commands: text (slot ABI). Results allocated through the channel. */
#include <ctype.h>
#include <string.h>
#include "kiln_core.h"

static const char *nz(const char *s){ return s?s:""; }
static char *astr(long len){ return (char*)kn_malloc(len+1); }

/* Text equality compares CONTENT, not pointers: two separately-built strings
 * with the same characters must be equal. Returns SDT_BOOL. */
void kn_text_eq(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c;
    const char *a = nz(kn_arg_text(argv, 0)), *b = nz(kn_arg_text(argv, 1));
    r->tag = KN_SDT_BOOL;
    r->v.i32 = (strcmp(a, b) == 0) ? 1 : 0;
}

/* --- UTF-8 helpers -------------------------------------------------------
 * Text is UTF-8, so positions and counts are measured in CHARACTERS. Measuring
 * in bytes leaks the encoding into every program: a name with an accent in it
 * would have the wrong length, and cutting at a byte offset would split a
 * character and produce text that is no longer valid UTF-8. */
static long kn_u8_len(const char *s, long i, long n){
    unsigned char b = (unsigned char)s[i];
    long len = 1;
    if((b&0xE0)==0xC0) len=2; else if((b&0xF0)==0xE0) len=3; else if((b&0xF8)==0xF0) len=4;
    return (i+len>n) ? 1 : len;              /* truncated: treat as one byte */
}
/* Byte offset of character index `chars`, clamped to the end. */
static long kn_u8_offset(const char *s, long n, long chars){
    long i=0;
    while(i<n && chars>0){ i += kn_u8_len(s,i,n); chars--; }
    return i;
}
static long kn_u8_count(const char *s, long n){
    long i=0, c=0;
    while(i<n){ i += kn_u8_len(s,i,n); c++; }
    return c;
}

/* Characters, not bytes. */
void kn_length(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0));
    kn_ret_int(r,(int)kn_u8_count(s,(long)strlen(s)));
}

void kn_uppercase(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0)); long n=(long)strlen(s); char*o=astr(n);
    for(long i=0;i<n;i++) o[i]=(char)toupper((unsigned char)s[i]); o[n]='\0'; kn_ret_text(r,o);
}
void kn_lowercase(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0)); long n=(long)strlen(s); char*o=astr(n);
    for(long i=0;i<n;i++) o[i]=(char)tolower((unsigned char)s[i]); o[n]='\0'; kn_ret_text(r,o);
}
void kn_trim(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0));
    const char*a=s; while(*a && isspace((unsigned char)*a)) a++;
    const char*e=s+strlen(s); while(e>a && isspace((unsigned char)e[-1])) e--;
    long n=e-a; char*o=astr(n); memcpy(o,a,n); o[n]='\0'; kn_ret_text(r,o);
}
/* Start and count are in characters, so a slice can never split one. */
void kn_substr(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0));
    int start=kn_arg_int(argv,1), count=kn_arg_int(argv,2);
    long len=(long)strlen(s);
    /* Positions count from 1.  A start below that is clamped rather than
     * rejected: the text commands have never failed, and a substring is not
     * where to start. */
    if(start<1)start=1; if(count<0)count=0;
    long from=kn_u8_offset(s,len,start-1);
    long to=kn_u8_offset(s,len,(long)start-1+count);
    long n=to-from;
    char*o=astr(n); memcpy(o,s+from,(size_t)n); o[n]='\0'; kn_ret_text(r,o);
}
void kn_find(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*h=nz(kn_arg_text(argv,0)), *n=nz(kn_arg_text(argv,1));
    const char*hit=strstr(h,n);
    /* A CHARACTER position counting from 1, and 0 when absent.  It returned a
     * byte offset before, which disagreed with every other position in this
     * file the moment the text was not ASCII. */
    kn_ret_int(r, hit ? (int)(kn_u8_count(h,(long)(hit-h)) + 1) : 0);
}
void kn_concat(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*a=nz(kn_arg_text(argv,0)),*b=nz(kn_arg_text(argv,1));
    long la=(long)strlen(a),lb=(long)strlen(b); char*o=astr(la+lb);
    memcpy(o,a,la); memcpy(o+la,b,lb); o[la+lb]='\0'; kn_ret_text(r,o);
}
void kn_repeat(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0)); int times=kn_arg_int(argv,1); if(times<0)times=0;
    long n=(long)strlen(s), total=n*(long)times; char*o=astr(total); char*p=o;
    for(int i=0;i<times;i++){ memcpy(p,s,n); p+=n; } *p='\0'; kn_ret_text(r,o);
}
/* Reverses CHARACTERS, not bytes. Text is UTF-8: reversing bytes splits every
 * multi-byte character into its pieces and emits them backwards, which is not
 * a reversed string but a corrupt one. Each character's bytes are copied as a
 * unit; a malformed byte is copied alone so invalid input degrades rather than
 * spreading. */
void kn_reverse(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; const char*s=nz(kn_arg_text(argv,0)); long n=(long)strlen(s);
    char*o=astr(n); long w=n;
    for(long i=0;i<n;){
        long len=1; unsigned char b=(unsigned char)s[i];
        if((b&0xE0)==0xC0) len=2; else if((b&0xF0)==0xE0) len=3; else if((b&0xF8)==0xF0) len=4;
        if(i+len>n) len=1;                 /* truncated sequence: copy the byte alone */
        w-=len; memcpy(o+w,s+i,(size_t)len); i+=len;
    }
    o[n]='\0'; kn_ret_text(r,o);
}
void kn_replace(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c;
    const char*s=nz(kn_arg_text(argv,0)),*from=nz(kn_arg_text(argv,1)),*to=nz(kn_arg_text(argv,2));
    long flen=(long)strlen(from);
    if(flen==0){ long n=(long)strlen(s); char*o=astr(n); memcpy(o,s,n+1); kn_ret_text(r,o); return; }
    long tlen=(long)strlen(to), count=0;
    for(const char*p=s;(p=strstr(p,from));p+=flen) count++;
    long slen=(long)strlen(s), outlen=slen+count*(tlen-flen);
    char*o=astr(outlen); char*w=o; const char*p=s;
    for(;;){ const char*hit=strstr(p,from);
        if(!hit){ long rest=(long)strlen(p); memcpy(w,p,rest); w+=rest; break; }
        long chunk=hit-p; memcpy(w,p,chunk); w+=chunk; memcpy(w,to,tlen); w+=tlen; p=hit+flen; }
    *w='\0'; kn_ret_text(r,o);
}
