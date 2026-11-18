CLASS zsample_atchk DEFINITION PUBLIC FINAL CREATE PUBLIC.
  PUBLIC SECTION.
    METHODS run.
ENDCLASS.

CLASS zsample_atchk IMPLEMENTATION.
  METHOD run.
    DATA unused TYPE i.
    DATA clients TYPE STANDARD TABLE OF t000.
    SELECT * FROM t000 INTO TABLE @clients.
    LOOP AT clients INTO DATA(client).
      SELECT SINGLE * FROM t000 WHERE mandt = @client-mandt INTO @DATA(again).
    ENDLOOP.
    TRY.
        DATA(ratio) = 1 / lines( clients ).
      CATCH cx_root.
    ENDTRY.
    IF clients IS INITIAL.
      MESSAGE 'No clients found!' TYPE 'I'.
    ENDIF.
  ENDMETHOD.
ENDCLASS.
